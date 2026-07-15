//! Win32 无边框全屏窗口
//!
//! 创建/销毁/绘制发送端的视觉帧显示窗口。
//! 窗口样式：WS_POPUP（无边框）、WS_EX_TOPMOST（置顶）。
//! v0.5: 真实全屏模式 —— 窗口使用完整屏幕尺寸（screen_w × screen_h）。
//! v0.6: 编码图案四周留 FRAME_MARGIN_PX 黑边，便于接收端定位。
//! modules_per_side_w / modules_per_side_h 由 (窗口尺寸 - 2×FRAME_MARGIN_PX) / module_pixels 动态计算。

use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::common::constants::RGBA_CHANNELS;
use crate::common::error::{OptransError, OptransResult};
use crate::win32::ffi;
use crate::win32::helpers::{check_hwnd, last_error, to_wide};

use log::info;

/// 窗口额外内存中存储用户数据的偏移（64 位指针 = 8 字节）
const USERDATA_OFFSET: i32 = 0;

/// 全局标志：窗口是否已关闭
static WINDOW_CLOSED: AtomicBool = AtomicBool::new(false);

/// 发送端窗口结构
pub struct SenderWindow {
    /// 窗口句柄
    hwnd: ffi::HWND,
    /// 窗口宽度（像素，= 屏幕宽度 screen_w）
    width: i32,
    /// 窗口高度（像素，= 屏幕高度 screen_h）
    height: i32,
    /// 每模块像素数（用于从 modules 反算帧像素尺寸）
    module_pixels: u32,
    /// 帧边距像素数
    frame_margin: u32,
    /// 水平方向模块数
    pub modules_per_side_w: u32,
    /// 垂直方向模块数
    pub modules_per_side_h: u32,
    /// 位图缓冲区（RGBA，width × height = screen_w × screen_h）
    bitmap_buffer: Vec<u8>,
}

/// 窗口过程回调
unsafe extern "system" fn wnd_proc(
    hwnd: ffi::HWND,
    msg: ffi::UINT,
    wparam: ffi::WPARAM,
    lparam: ffi::LPARAM,
) -> ffi::LRESULT {
    match msg {
        ffi::WM_PAINT => {
            let mut ps: ffi::PAINTSTRUCT = unsafe { std::mem::zeroed() };
            let hdc = unsafe { ffi::BeginPaint(hwnd, &mut ps) };
            if hdc != ffi::NULL as ffi::HDC {
                // 获取窗口关联的位图缓冲区（通过 cbWndExtra 分配的额外窗口内存）
                let pixels_ptr =
                    unsafe { ffi::GetWindowLongPtrW(hwnd, USERDATA_OFFSET) } as *const u8;
                if !pixels_ptr.is_null() {
                    let mut rect: ffi::RECT = unsafe { std::mem::zeroed() };
                    unsafe { ffi::GetClientRect(hwnd, &mut rect) };
                    let w = rect.right - rect.left;
                    let h = rect.bottom - rect.top;

                    // 构建 BITMAPINFO
                    let bmi_header = ffi::BITMAPINFOHEADER {
                        biSize: std::mem::size_of::<ffi::BITMAPINFOHEADER>() as ffi::DWORD,
                        biWidth: w,
                        biHeight: -h, // 负值 = 自上而下
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: ffi::BI_RGB,
                        ..Default::default()
                    };

                    let bmi = ffi::BITMAPINFO {
                        bmiHeader: bmi_header,
                        bmiColors: [ffi::RGBQUAD {
                            rgbBlue: 0,
                            rgbGreen: 0,
                            rgbRed: 0,
                            rgbReserved: 0,
                        }],
                    };

                    unsafe {
                        ffi::SetDIBitsToDevice(
                            hdc,
                            0,
                            0,
                            w as ffi::DWORD,
                            h as ffi::DWORD,
                            0,
                            0,
                            0,
                            h as ffi::UINT,
                            pixels_ptr as *const c_void,
                            &bmi,
                            ffi::DIB_RGB_COLORS,
                        );
                    }
                }
                unsafe { ffi::EndPaint(hwnd, &ps) };
            }
            0
        }
        ffi::WM_ERASEBKGND => {
            // 防止闪烁：返回非零表示已处理
            1
        }
        ffi::WM_SETCURSOR => {
            // 隐藏全屏窗口上的光标，防止干扰接收端截图
            1
        }
        ffi::WM_WINDOWPOSCHANGING => {
            // 强制保持 TOPMOST，防止被其他窗口覆盖（如系统对话框）
            let pos = lparam as *mut ffi::WINDOWPOS;
            if !pos.is_null() {
                unsafe {
                    (*pos).flags &= !(ffi::SWP_NOZORDER);
                    (*pos).flags |= ffi::SWP_NOMOVE | ffi::SWP_NOSIZE;
                    (*pos).hwndInsertAfter = ffi::HWND_TOPMOST;
                }
            }
            0
        }
        ffi::WM_DESTROY => {
            WINDOW_CLOSED.store(true, Ordering::SeqCst);
            unsafe { ffi::PostQuitMessage(0) };
            0
        }
        ffi::WM_CLOSE => {
            WINDOW_CLOSED.store(true, Ordering::SeqCst);
            unsafe { ffi::PostQuitMessage(0) };
            0
        }
        _ => unsafe { ffi::DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

impl SenderWindow {
    /// 创建无边框真实全屏窗口（screen_w × screen_h）
    ///
    /// # 参数
    /// - `module_pixels`: 每模块像素数
    /// - `frame_margin`: 帧边距像素数
    pub fn create(module_pixels: u32, frame_margin: u32) -> OptransResult<Self> {
        let screen_w = unsafe { ffi::GetSystemMetrics(ffi::SM_CXSCREEN) };
        let screen_h = unsafe { ffi::GetSystemMetrics(ffi::SM_CYSCREEN) };
        let modules_w = (screen_w as u32 - frame_margin * 2) / module_pixels;
        let modules_h = (screen_h as u32 - frame_margin * 2) / module_pixels;
        Self::create_sized(module_pixels, modules_w, modules_h, frame_margin)
    }

    /// 创建指定模块尺寸的居中窗口（动态帧尺寸）
    pub fn create_sized(module_pixels: u32, modules_w: u32, modules_h: u32, frame_margin: u32) -> OptransResult<Self> {
        // 重置全局标志
        WINDOW_CLOSED.store(false, Ordering::SeqCst);

        let hinstance = unsafe { ffi::GetModuleHandleW(ptr::null()) };
        if hinstance == ffi::NULL as ffi::HINSTANCE {
            return Err(OptransError::WindowCreation(format!(
                "GetModuleHandleW 失败: {}",
                last_error()
            )));
        }

        let class_name = to_wide("OptransSenderWindow");
        let window_title = to_wide("optrans - Optical Transfer");

        // 窗口尺寸 = 帧图案尺寸 + 2×frame_margin 边距
        let window_w = (modules_w * module_pixels + frame_margin * 2) as i32;
        let window_h = (modules_h * module_pixels + frame_margin * 2) as i32;

        // 居中显示
        let screen_w = unsafe { ffi::GetSystemMetrics(ffi::SM_CXSCREEN) };
        let screen_h = unsafe { ffi::GetSystemMetrics(ffi::SM_CYSCREEN) };
        let win_x = ((screen_w - window_w) / 2).max(0);
        let win_y = ((screen_h - window_h) / 2).max(0);

        let modules_per_side_w = modules_w;
        let modules_per_side_h = modules_h;

        if modules_per_side_w == 0 || modules_per_side_h == 0 {
            return Err(OptransError::WindowCreation(format!(
                "模块尺寸为 0: modules={}x{} (module_pixels={})",
                modules_w, modules_h, module_pixels
            )));
        }

        // 帧图案实际像素尺寸（= modules_per_side × module_pixels，≤ 窗口尺寸 - 2×FRAME_MARGIN_PX）
        let adjusted_w = (modules_per_side_w * module_pixels) as i32;
        let adjusted_h = (modules_per_side_h * module_pixels) as i32;

        // 注册窗口类 —— 显式初始化所有字段
        let wc = ffi::WNDCLASSEXW {
            cbSize: std::mem::size_of::<ffi::WNDCLASSEXW>() as ffi::UINT,
            style: ffi::CS_HREDRAW | ffi::CS_VREDRAW | ffi::CS_OWNDC,
            lpfnWndProc: Some(wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: std::mem::size_of::<isize>() as i32, // 为 SetWindowLongPtrW 分配指针空间
            hInstance: hinstance,
            hIcon: ffi::NULL as ffi::HICON,
            hCursor: ffi::NULL as ffi::HCURSOR,
            hbrBackground: ffi::NULL as ffi::HBRUSH, // 我们自己处理背景
            lpszMenuName: ptr::null(),
            lpszClassName: class_name.as_ptr(),
            hIconSm: ffi::NULL as ffi::HICON,
        };

        let atom = unsafe { ffi::RegisterClassExW(&wc) };
        if atom == 0 {
            return Err(OptransError::WindowCreation(format!(
                "RegisterClassExW 失败: {}",
                last_error()
            )));
        }

        // 创建窗口（居中显示，尺寸 = window_w × window_h）
        let hwnd = unsafe {
            ffi::CreateWindowExW(
                ffi::WS_EX_TOPMOST,
                class_name.as_ptr(),
                window_title.as_ptr(),
                ffi::WS_POPUP | ffi::WS_VISIBLE,
                win_x, // 居中 X
                win_y, // 居中 Y
                window_w,
                window_h,
                ffi::NULL as ffi::HWND,
                ffi::NULL,
                hinstance,
                ptr::null(),
            )
        };

        check_hwnd(hwnd)?;

        // 分配位图缓冲区（全屏尺寸 = screen_w × screen_h，初始化为全黑）
        let bitmap_buffer =
            vec![0u8; (window_w as usize) * (window_h as usize) * RGBA_CHANNELS];

        // 将位图缓冲区指针存储到窗口额外内存中
        unsafe {
            ffi::SetWindowLongPtrW(hwnd, USERDATA_OFFSET, bitmap_buffer.as_ptr() as isize);
        }

        let mut window = SenderWindow {
            hwnd,
            width: window_w,
            height: window_h,
            module_pixels,
            frame_margin,
            modules_per_side_w,
            modules_per_side_h,
            bitmap_buffer,
        };

        // 渲染醒目的初始帧：全黑底色 + 白色窗口边缘边框
        window.render_initial_frame();

        // 强制显示并置顶
        window.show();

        // 隐藏系统光标，防止叠加在编码图案上干扰接收端截图
        unsafe { ffi::ShowCursor(ffi::FALSE) };

        // 通知用户窗口位置和帧尺寸
        info!(
            "全屏窗口已显示 (0,0) {}×{}，帧图案 {}×{}（{}px边距），modules=({}w×{}h), module_pixels={}",
            window_w, window_h, adjusted_w, adjusted_h,
            frame_margin,
            modules_per_side_w, modules_per_side_h, module_pixels
        );

        Ok(window)
    }

    /// 渲染初始可见帧（全黑 + 白色窗口边缘边框），确保用户能看到窗口
    fn render_initial_frame(&mut self) {
        let w = self.width as usize;
        let h = self.height as usize;
        let buf = &mut self.bitmap_buffer;

        // 填充整个缓冲区为黑色
        buf.fill(0);

        // 绘制白色窗口边缘边框（4 像素宽）
        let border = 4usize;
        for y in 0..h {
            for x in 0..w {
                let in_border = x < border
                    || x >= w - border
                    || y < border
                    || y >= h - border;
                if in_border {
                    let idx = (y * w + x) * RGBA_CHANNELS;
                    buf[idx] = 255;     // R
                    buf[idx + 1] = 255; // G
                    buf[idx + 2] = 255; // B
                    buf[idx + 3] = 255; // A
                }
            }
        }

        // 更新窗口用户数据指针（缓冲区地址未变，但显式更新以策安全）
        unsafe {
            ffi::SetWindowLongPtrW(
                self.hwnd,
                USERDATA_OFFSET,
                self.bitmap_buffer.as_ptr() as isize,
            );
        }
    }

    /// 更新位图缓冲区并触发重绘
    ///
    /// 帧像素（adjusted_w × adjusted_h）居中写入 bitmap_buffer，四周留黑边。
    /// `pixels` 是 adjusted_w × adjusted_h 的 RGBA 像素数据。
    pub fn update_bitmap(&mut self, pixels: &[u8]) {
        let window_w = self.width as usize;
        let frame_w = (self.modules_per_side_w * self.module_pixels) as usize;
        let frame_h = (self.modules_per_side_h * self.module_pixels) as usize;
        let margin = self.frame_margin as usize;

        // 清空整个缓冲区为黑色
        self.bitmap_buffer.fill(0);

        // 将帧像素逐行写入 bitmap_buffer，起始偏移 (margin, margin)
        let expected_len = frame_w * frame_h * RGBA_CHANNELS;
        let copy_len = pixels.len().min(expected_len);

        let src_stride = frame_w * RGBA_CHANNELS;
        let dst_stride = window_w * RGBA_CHANNELS;
        let dst_base = margin * dst_stride + margin * RGBA_CHANNELS; // 偏移 (margin, margin)

        let rows = copy_len.div_ceil(src_stride);
        for row in 0..rows {
            let row_start = row * src_stride;
            let row_end = (row_start + src_stride).min(copy_len);
            let row_data = &pixels[row_start..row_end];
            let dst_start = dst_base + row * dst_stride;
            let dst_end = dst_start + row_data.len();
            self.bitmap_buffer[dst_start..dst_end].copy_from_slice(row_data);
        }

        // 更新窗口用户数据指针
        unsafe {
            ffi::SetWindowLongPtrW(
                self.hwnd,
                USERDATA_OFFSET,
                self.bitmap_buffer.as_ptr() as isize,
            );
        }

        // 触发 WM_PAINT
        self.invalidate();
    }

    /// 显示窗口并强制置顶
    pub fn show(&mut self) {
        unsafe {
            ffi::ShowWindow(self.hwnd, ffi::SW_SHOW);
            // 强制置顶：确保窗口在所有其他窗口之上（修复远程桌面等场景中被覆盖的问题）
            ffi::SetWindowPos(
                self.hwnd,
                ffi::HWND_TOPMOST,
                0,
                0,
                0,
                0,
                ffi::SWP_NOMOVE | ffi::SWP_NOSIZE | ffi::SWP_SHOWWINDOW,
            );
            ffi::UpdateWindow(self.hwnd);
        }
    }

    /// 使窗口客户区无效，触发重绘，并等待像素上屏。
    ///
    /// 三步保证帧已展示：
    /// 1. `InvalidateRect` + `UpdateWindow` — 同步发送 WM_PAINT，SetDIBitsToDevice 执行
    /// 2. `GdiFlush` — 刷新 GDI 命令缓冲区，确保绘制命令已提交到显示驱动
    /// 3. `DwmFlush` — 阻塞等待 DWM 完成下一次合成，确保像素已从 framebuffer
    ///    提交到屏幕。远程桌面场景下，这保证远程桌面协议捕获到的是最新帧
    ///    而非上一帧（DWM 未合成时捕获会拿到旧内容）。
    fn invalidate(&self) {
        unsafe {
            // 标记整个客户区为"脏"，以便 UpdateWindow 真正发送 WM_PAINT
            ffi::InvalidateRect(self.hwnd, ptr::null(), 1);
            // 立即发送 WM_PAINT（跳过消息队列）
            ffi::UpdateWindow(self.hwnd);
            // 强制 GDI 立即执行所有排队的绘制命令
            ffi::GdiFlush();
            // 等待 DWM 完成合成，确保像素已上屏
            // DWM 按 vsync 刷新（~60Hz），此调用阻塞至下一次合成完成
            let _ = ffi::DwmFlush();
        }
    }

    /// 处理 Windows 消息
    ///
    /// 返回 `true` 如果应该继续运行，`false` 如果收到了退出消息。
    pub fn process_messages(&self) -> bool {
        let mut msg: ffi::MSG = unsafe { std::mem::zeroed() };

        while unsafe {
            ffi::PeekMessageW(&mut msg, ffi::NULL as ffi::HWND, 0, 0, ffi::PM_REMOVE)
        } != 0
        {
            if msg.message == ffi::WM_QUIT {
                return false;
            }
            unsafe {
                ffi::TranslateMessage(&msg);
                ffi::DispatchMessageW(&msg);
            }
        }

        // 检查窗口是否已关闭
        if WINDOW_CLOSED.load(Ordering::SeqCst) {
            return false;
        }

        true
    }

    /// 在超时时间内处理消息并等待
    ///
    /// 使用 `MsgWaitForMultipleObjects` 阻塞等待，线程在无消息时真正挂起，零 CPU 占用。
    /// 消息到达时立即唤醒处理，确保 UI 响应性。
    ///
    /// 返回 `true` 如果应该继续运行，`false` 如果收到了退出消息。
    pub fn wait_with_message_pump(&self, ms: u64) -> bool {
        loop {
            if !self.process_messages() {
                return false;
            }
            let ret = unsafe {
                ffi::MsgWaitForMultipleObjects(
                    0,
                    std::ptr::null(),
                    0,
                    ms as u32,
                    ffi::QS_ALLINPUT,
                )
            };
            if ret != ffi::WAIT_OBJECT_0 {
                break;
            }
        }
        true
    }
}

impl Drop for SenderWindow {
    fn drop(&mut self) {
        // 恢复系统光标
        unsafe { ffi::ShowCursor(ffi::TRUE) };
        if self.hwnd != ffi::NULL as ffi::HWND {
            unsafe {
                ffi::DestroyWindow(self.hwnd);
            }
            self.hwnd = ffi::NULL as ffi::HWND;
        }
    }
}
