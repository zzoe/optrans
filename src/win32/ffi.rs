//! Win32 FFI 声明
//!
//! 手工 `extern "system"` 声明所有需要的 Win32 API，
//! 不依赖 `windows-sys` crate。

#![allow(non_snake_case)]
#![allow(non_camel_case_types)]
#![allow(dead_code)]

use std::ffi::c_void;

// ============================================================================
// 基础类型别名
// ============================================================================

/// 窗口句柄
pub type HWND = isize;
/// 设备上下文句柄
pub type HDC = isize;
/// 位图句柄
pub type HBITMAP = isize;
/// 实例句柄
pub type HINSTANCE = isize;
/// 图标句柄
pub type HICON = isize;
/// 光标句柄
pub type HCURSOR = isize;
/// 画刷句柄
pub type HBRUSH = isize;
/// GDI 对象句柄（通用）
pub type HGDIOBJ = isize;
/// 消息参数
pub type LPARAM = isize;
/// 消息参数
pub type WPARAM = usize;
/// 窗口过程返回值
pub type LRESULT = isize;
/// 布尔类型（Windows BOOL）
pub type BOOL = i32;
/// COM/DWM 函数返回值
pub type HRESULT = i32;
/// 双字
pub type DWORD = u32;
/// 无符号整数
pub type UINT = u32;
/// 长整数
pub type LONG = i32;
/// 字
pub type WORD = u16;
/// 字节
pub type BYTE = u8;
/// 原子
pub type ATOM = u16;
/// 颜色值
pub type COLORREF = DWORD;
/// 窗口过程函数指针
pub type WNDPROC = Option<
    unsafe extern "system" fn(hwnd: HWND, msg: UINT, wparam: WPARAM, lparam: LPARAM) -> LRESULT,
>;

// ============================================================================
// 常量
// ============================================================================

/// 通用布尔值
pub(crate) const FALSE: BOOL = 0;
pub(crate) const TRUE: BOOL = 1;

/// NULL 句柄/指针
pub(crate) const NULL: isize = 0;

// 窗口样式
pub(crate) const WS_POPUP: DWORD = 0x80000000;
pub(crate) const WS_VISIBLE: DWORD = 0x10000000;
pub(crate) const WS_EX_TOPMOST: DWORD = 0x00000008;

// 窗口消息
pub(crate) const WM_PAINT: UINT = 0x000F;
pub(crate) const WM_CLOSE: UINT = 0x0010;
pub(crate) const WM_DESTROY: UINT = 0x0002;
pub(crate) const WM_QUIT: UINT = 0x0012;
pub(crate) const WM_ERASEBKGND: UINT = 0x0014;
pub(crate) const WM_SETCURSOR: UINT = 0x0020;
pub(crate) const WM_WINDOWPOSCHANGING: UINT = 0x0046;

// ShowWindow 命令
pub(crate) const SW_SHOW: i32 = 5;
pub(crate) const SW_HIDE: i32 = 0;

// 窗口定位
pub(crate) const HWND_TOPMOST: HWND = -1;
pub(crate) const SWP_NOMOVE: UINT = 0x0002;
pub(crate) const SWP_NOSIZE: UINT = 0x0001;
pub(crate) const SWP_SHOWWINDOW: UINT = 0x0040;
pub(crate) const SWP_NOZORDER: UINT = 0x0004;

// 系统指标
pub(crate) const SM_CXSCREEN: i32 = 0;
pub(crate) const SM_CYSCREEN: i32 = 1;
pub(crate) const SM_XVIRTUALSCREEN: i32 = 76;
pub(crate) const SM_YVIRTUALSCREEN: i32 = 77;
pub(crate) const SM_CXVIRTUALSCREEN: i32 = 78;
pub(crate) const SM_CYVIRTUALSCREEN: i32 = 79;

// 位图相关
pub(crate) const BI_RGB: DWORD = 0;
pub(crate) const DIB_RGB_COLORS: UINT = 0;

// 窗口类样式
pub(crate) const CS_HREDRAW: UINT = 0x0002;
pub(crate) const CS_VREDRAW: UINT = 0x0001;
pub(crate) const CS_OWNDC: UINT = 0x0020;

// PeekMessage 标志
pub(crate) const PM_REMOVE: UINT = 0x0001;

// MsgWaitForMultipleObjects 返回值
pub(crate) const WAIT_OBJECT_0: DWORD = 0;

// MsgWaitForMultipleObjects 唤醒掩码：任意消息类型
pub(crate) const QS_ALLINPUT: UINT = 0x04FF;

// SetThreadExecutionState 标志
/// 持续生效：状态保持到下次调用清除
pub(crate) const ES_CONTINUOUS: DWORD = 0x80000000;

/// 阻止系统睡眠
pub(crate) const ES_SYSTEM_REQUIRED: DWORD = 0x00000001;

/// 阻止显示器关闭与锁屏
pub(crate) const ES_DISPLAY_REQUIRED: DWORD = 0x00000002;

// ============================================================================
// 结构体
// ============================================================================

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct POINT {
    pub x: LONG,
    pub y: LONG,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SIZE {
    pub cx: LONG,
    pub cy: LONG,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RECT {
    pub left: LONG,
    pub top: LONG,
    pub right: LONG,
    pub bottom: LONG,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MSG {
    pub hwnd: HWND,
    pub message: UINT,
    pub wParam: WPARAM,
    pub lParam: LPARAM,
    pub time: DWORD,
    pub pt: POINT,
}

#[repr(C)]
#[derive(Debug, Clone)]
pub struct WNDCLASSEXW {
    pub cbSize: UINT,
    pub style: UINT,
    pub lpfnWndProc: WNDPROC,
    pub cbClsExtra: i32,
    pub cbWndExtra: i32,
    pub hInstance: HINSTANCE,
    pub hIcon: HICON,
    pub hCursor: HCURSOR,
    pub hbrBackground: HBRUSH,
    pub lpszMenuName: *const u16,
    pub lpszClassName: *const u16,
    pub hIconSm: HICON,
}

impl Default for WNDCLASSEXW {
    fn default() -> Self {
        WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as UINT,
            style: 0,
            lpfnWndProc: None,
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: NULL as HINSTANCE,
            hIcon: NULL as HICON,
            hCursor: NULL as HCURSOR,
            hbrBackground: NULL as HBRUSH,
            lpszMenuName: std::ptr::null(),
            lpszClassName: std::ptr::null(),
            hIconSm: NULL as HICON,
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PAINTSTRUCT {
    pub hdc: HDC,
    pub fErase: BOOL,
    pub rcPaint: RECT,
    pub fRestore: BOOL,
    pub fIncUpdate: BOOL,
    pub rgbReserved: [BYTE; 32],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct BITMAPINFOHEADER {
    pub biSize: DWORD,
    pub biWidth: LONG,
    pub biHeight: LONG,
    pub biPlanes: WORD,
    pub biBitCount: WORD,
    pub biCompression: DWORD,
    pub biSizeImage: DWORD,
    pub biXPelsPerMeter: LONG,
    pub biYPelsPerMeter: LONG,
    pub biClrUsed: DWORD,
    pub biClrImportant: DWORD,
}

impl Default for BITMAPINFOHEADER {
    fn default() -> Self {
        BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as DWORD,
            biWidth: 0,
            biHeight: 0,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RGBQUAD {
    pub rgbBlue: BYTE,
    pub rgbGreen: BYTE,
    pub rgbRed: BYTE,
    pub rgbReserved: BYTE,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct BITMAPINFO {
    pub bmiHeader: BITMAPINFOHEADER,
    pub bmiColors: [RGBQUAD; 1],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct WINDOWPOS {
    pub hwnd: HWND,
    pub hwndInsertAfter: HWND,
    pub x: i32,
    pub y: i32,
    pub cx: i32,
    pub cy: i32,
    pub flags: UINT,
}

// ============================================================================
// kernel32.dll
// ============================================================================

#[link(name = "kernel32")]
unsafe extern "system" {
    /// 获取当前模块句柄
    pub fn GetModuleHandleW(lpModuleName: *const u16) -> HINSTANCE;

    /// 获取最后一次错误码
    pub fn GetLastError() -> DWORD;

    /// 格式化错误消息
    pub fn FormatMessageW(
        dwFlags: DWORD,
        lpSource: *const c_void,
        dwMessageId: DWORD,
        dwLanguageId: DWORD,
        lpBuffer: *mut u16,
        nSize: DWORD,
        Arguments: *const c_void,
    ) -> DWORD;

    /// 休眠指定毫秒数
    pub fn Sleep(dwMilliseconds: DWORD);

    /// 设置线程执行状态（阻止系统睡眠/熄屏/锁屏）
    ///
    /// 返回上一个执行状态（非 0 表示成功，0 表示失败）。
    pub fn SetThreadExecutionState(esFlags: DWORD) -> DWORD;
}

// ============================================================================
// user32.dll
// ============================================================================

#[link(name = "user32")]
unsafe extern "system" {
    /// 注册窗口类
    pub fn RegisterClassExW(lpWndClass: *const WNDCLASSEXW) -> ATOM;

    /// 创建窗口
    pub fn CreateWindowExW(
        dwExStyle: DWORD,
        lpClassName: *const u16,
        lpWindowName: *const u16,
        dwStyle: DWORD,
        X: i32,
        Y: i32,
        nWidth: i32,
        nHeight: i32,
        hWndParent: HWND,
        hMenu: isize,
        hInstance: HINSTANCE,
        lpParam: *const c_void,
    ) -> HWND;

    /// 显示窗口
    pub fn ShowWindow(hWnd: HWND, nCmdShow: i32) -> BOOL;

    /// 更新窗口
    pub fn UpdateWindow(hWnd: HWND) -> BOOL;

    /// 获取消息（阻塞）
    pub fn GetMessageW(
        lpMsg: *mut MSG,
        hWnd: HWND,
        wMsgFilterMin: UINT,
        wMsgFilterMax: UINT,
    ) -> BOOL;

    /// 查看消息（非阻塞）
    pub fn PeekMessageW(
        lpMsg: *mut MSG,
        hWnd: HWND,
        wMsgFilterMin: UINT,
        wMsgFilterMax: UINT,
        wRemoveMsg: UINT,
    ) -> BOOL;

    /// 翻译消息
    pub fn TranslateMessage(lpMsg: *const MSG) -> BOOL;

    /// 分发消息到窗口过程
    pub fn DispatchMessageW(lpMsg: *const MSG) -> LRESULT;

    /// 开始绘制
    pub fn BeginPaint(hWnd: HWND, lpPaint: *mut PAINTSTRUCT) -> HDC;

    /// 结束绘制
    pub fn EndPaint(hWnd: HWND, lpPaint: *const PAINTSTRUCT) -> BOOL;

    /// 设置窗口位置
    pub fn SetWindowPos(
        hWnd: HWND,
        hWndInsertAfter: HWND,
        X: i32,
        Y: i32,
        cx: i32,
        cy: i32,
        uFlags: UINT,
    ) -> BOOL;

    /// 销毁窗口
    pub fn DestroyWindow(hWnd: HWND) -> BOOL;

    /// 发送退出消息
    pub fn PostQuitMessage(nExitCode: i32);

    /// 默认窗口过程
    pub fn DefWindowProcW(hWnd: HWND, Msg: UINT, wParam: WPARAM, lParam: LPARAM) -> LRESULT;

    /// 获取系统指标
    pub fn GetSystemMetrics(nIndex: i32) -> i32;

    /// 获取桌面 DC
    pub fn GetDC(hWnd: HWND) -> HDC;

    /// 释放 DC
    pub fn ReleaseDC(hWnd: HWND, hDC: HDC) -> i32;

    /// 获取桌面窗口句柄
    pub fn GetDesktopWindow() -> HWND;

    /// 获取客户区矩形
    pub fn GetClientRect(hWnd: HWND, lpRect: *mut RECT) -> BOOL;

    /// 获取窗口 DC（含非客户区）
    pub fn GetWindowDC(hWnd: HWND) -> HDC;

    /// 获取窗口长属性
    pub fn GetWindowLongPtrW(hWnd: HWND, nIndex: i32) -> isize;

    /// 设置窗口长属性
    pub fn SetWindowLongPtrW(hWnd: HWND, nIndex: i32, dwNewLong: isize) -> isize;

    /// 使窗口客户区矩形无效，触发 WM_PAINT
    pub fn InvalidateRect(hWnd: HWND, lpRect: *const RECT, bErase: BOOL) -> BOOL;

    /// 显示/隐藏系统光标
    ///
    /// `ShowCursor(FALSE)` 隐藏光标，`ShowCursor(TRUE)` 恢复。
    /// 返回新的显示计数。
    pub fn ShowCursor(bShow: BOOL) -> i32;

    /// 设置进程为 DPI 感知（Vista+，Win7 兼容）
    pub fn SetProcessDPIAware() -> BOOL;

    /// 等待消息到达或超时——线程真正休眠，零 CPU 空转
    ///
    /// 替代 Sleep 轮询：当没有消息时，线程挂起直到超时或消息进入队列。
    /// 用于帧间隔等待，比 Sleep(10) 循环精确得多且不浪费 CPU。
    /// 返回 WAIT_OBJECT_0 表示有消息待处理，WAIT_TIMEOUT 表示超时。
    pub fn MsgWaitForMultipleObjects(
        nCount: DWORD,
        pHandles: *const c_void,
        fWaitAll: BOOL,
        dwMilliseconds: DWORD,
        dwWakeMask: DWORD,
    ) -> DWORD;
}

// ============================================================================
// gdi32.dll
// ============================================================================

#[link(name = "gdi32")]
unsafe extern "system" {
    /// 删除 GDI 对象
    pub fn DeleteObject(ho: HGDIOBJ) -> BOOL;

    /// 创建兼容 DC
    pub fn CreateCompatibleDC(hDC: HDC) -> HDC;

    /// 删除 DC
    pub fn DeleteDC(hDC: HDC) -> BOOL;

    /// 创建兼容位图
    pub fn CreateCompatibleBitmap(hDC: HDC, cx: i32, cy: i32) -> HBITMAP;

    /// 将 GDI 对象选入 DC
    pub fn SelectObject(hDC: HDC, h: HGDIOBJ) -> HGDIOBJ;

    /// 位块传输
    pub fn BitBlt(
        hdcDest: HDC,
        xDest: i32,
        yDest: i32,
        cx: i32,
        cy: i32,
        hdcSrc: HDC,
        xSrc: i32,
        ySrc: i32,
        rop: DWORD,
    ) -> BOOL;

    /// 获取 DIB 数据
    pub fn GetDIBits(
        hdc: HDC,
        hbm: HBITMAP,
        start: UINT,
        cLines: UINT,
        lpvBits: *mut c_void,
        lpbmi: *mut BITMAPINFO,
        usage: UINT,
    ) -> i32;

    /// 将 DIB 数据直接绘制到设备
    pub fn SetDIBitsToDevice(
        hdc: HDC,
        xDest: i32,
        yDest: i32,
        cx: DWORD,
        cy: DWORD,
        xSrc: i32,
        ySrc: i32,
        uStartScan: UINT,
        cScanLines: UINT,
        lpvBits: *const c_void,
        lpbmi: *const BITMAPINFO,
        fuColorUse: UINT,
    ) -> i32;

    /// 创建 DIB section（设备无关位图，可直接访问像素缓冲区）
    ///
    /// BitBlt 直接写入 ppvBits 指向的缓冲区，跳过 DDB↔DIB 格式转换，
    /// 保证像素值不丢失精度。
    pub fn CreateDIBSection(
        hdc: HDC,
        pbmi: *const BITMAPINFO,
        usage: UINT,
        ppvBits: *mut *mut c_void,
        hSection: isize,
        offset: DWORD,
    ) -> HBITMAP;

    /// 刷新 GDI 命令缓冲区
    ///
    /// 强制 GDI 立即执行所有排队的绘制命令。在远程桌面（BlastCodec/H.264/RDP）
    /// 环境下，调用此函数相当于告知编码器"当前帧绘制完毕，请立即编码发送"，
    /// 防止编码器将连续多帧合并为一次更新导致接收端丢帧。
    pub fn GdiFlush() -> BOOL;
}

// ============================================================================
// dwmapi.dll — Desktop Window Manager
// ============================================================================

#[link(name = "dwmapi")]
unsafe extern "system" {
    /// 阻塞直到 DWM 完成下一次合成并呈现。
    ///
    /// 调用此函数可保证窗口绘制内容已从 framebuffer 提交到屏幕，
    /// 避免"GDI 已提交但 DWM 未合成"的中间状态导致远程桌面捕获到旧帧。
    pub fn DwmFlush() -> HRESULT;
}

// ============================================================================
// 格式化消息常量
// ============================================================================

pub(crate) const FORMAT_MESSAGE_ALLOCATE_BUFFER: DWORD = 0x00000100;
pub(crate) const FORMAT_MESSAGE_FROM_SYSTEM: DWORD = 0x00001000;
pub(crate) const FORMAT_MESSAGE_IGNORE_INSERTS: DWORD = 0x00000200;
pub(crate) const LANG_NEUTRAL: DWORD = 0x00;
pub(crate) const SUBLANG_DEFAULT: DWORD = 0x01;

/// 组合语言 ID
pub(crate) const fn MAKELANGID(primary: DWORD, sub: DWORD) -> DWORD {
    (sub << 10) | primary
}
