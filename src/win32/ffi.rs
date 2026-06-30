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
pub const FALSE: BOOL = 0;
pub const TRUE: BOOL = 1;

/// NULL 句柄/指针
pub const NULL: isize = 0;

// 窗口样式
pub const WS_POPUP: DWORD = 0x80000000;
pub const WS_VISIBLE: DWORD = 0x10000000;
pub const WS_SYSMENU: DWORD = 0x00080000;
pub const WS_EX_TOPMOST: DWORD = 0x00000008;

// 窗口消息
pub const WM_PAINT: UINT = 0x000F;
pub const WM_CLOSE: UINT = 0x0010;
pub const WM_DESTROY: UINT = 0x0002;
pub const WM_KEYDOWN: UINT = 0x0100;
pub const WM_QUIT: UINT = 0x0012;
pub const WM_CREATE: UINT = 0x0001;
pub const WM_SIZE: UINT = 0x0005;
pub const WM_ERASEBKGND: UINT = 0x0014;
pub const WM_SETCURSOR: UINT = 0x0020;
pub const WM_WINDOWPOSCHANGING: UINT = 0x0046;

// 虚拟键码
pub const VK_ESCAPE: i32 = 0x1B;

// ShowWindow 命令
pub const SW_SHOW: i32 = 5;
pub const SW_HIDE: i32 = 0;

// 窗口定位
pub const HWND_TOPMOST: HWND = -1;
pub const SWP_NOMOVE: UINT = 0x0002;
pub const SWP_NOSIZE: UINT = 0x0001;
pub const SWP_SHOWWINDOW: UINT = 0x0040;
pub const SWP_NOZORDER: UINT = 0x0004;

// 系统指标
pub const SM_CXSCREEN: i32 = 0;
pub const SM_CYSCREEN: i32 = 1;

// 位图相关
pub const BI_RGB: DWORD = 0;
pub const DIB_RGB_COLORS: UINT = 0;
pub const SRCCOPY: DWORD = 0x00CC0020;

// 窗口类样式
pub const CS_HREDRAW: UINT = 0x0002;
pub const CS_VREDRAW: UINT = 0x0001;
pub const CS_OWNDC: UINT = 0x0020;

// 图片加载标志
pub const IMAGE_BITMAP: UINT = 0;
pub const LR_LOADFROMFILE: UINT = 0x0010;

// 窗口字段偏移
pub const GWL_STYLE: i32 = -16;
pub const GWL_EXSTYLE: i32 = -20;

// 显示器相关
pub const MONITOR_DEFAULTTOPRIMARY: DWORD = 1;

// PeekMessage 标志
pub const PM_REMOVE: UINT = 0x0001;
pub const PM_NOREMOVE: UINT = 0x0000;

// 色彩格式
pub const BLACKNESS: DWORD = 0x00000042;
pub const WHITENESS: DWORD = 0x00FF0062;

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

impl RECT {
    pub fn new(left: LONG, top: LONG, right: LONG, bottom: LONG) -> Self {
        RECT {
            left,
            top,
            right,
            bottom,
        }
    }

    pub fn width(&self) -> LONG {
        self.right - self.left
    }

    pub fn height(&self) -> LONG {
        self.bottom - self.top
    }
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
pub struct MONITORINFO {
    pub cbSize: DWORD,
    pub rcMonitor: RECT,
    pub rcWork: RECT,
    pub dwFlags: DWORD,
}

impl Default for MONITORINFO {
    fn default() -> Self {
        MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as DWORD,
            rcMonitor: RECT::new(0, 0, 0, 0),
            rcWork: RECT::new(0, 0, 0, 0),
            dwFlags: 0,
        }
    }
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

    /// 获取当前滴答计数（毫秒）
    pub fn GetTickCount() -> DWORD;
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

    /// 获取显示器信息
    pub fn GetMonitorInfoW(
        hMonitor: isize,
        lpmi: *mut MONITORINFO,
    ) -> BOOL;

    /// 从窗口获取显示器句柄
    pub fn MonitorFromWindow(hWnd: HWND, dwFlags: DWORD) -> isize;

    /// 加载图片
    pub fn LoadImageW(
        hInst: HINSTANCE,
        name: *const u16,
        typ: UINT,
        cx: i32,
        cy: i32,
        fuLoad: UINT,
    ) -> isize;

    /// 使窗口客户区矩形无效，触发 WM_PAINT
    pub fn InvalidateRect(hWnd: HWND, lpRect: *const RECT, bErase: BOOL) -> BOOL;

    /// 显示/隐藏系统光标
    ///
    /// `ShowCursor(FALSE)` 隐藏光标，`ShowCursor(TRUE)` 恢复。
    /// 返回新的显示计数。
    pub fn ShowCursor(bShow: BOOL) -> i32;

    /// 设置进程为 DPI 感知（Vista+，Win7 兼容）
    pub fn SetProcessDPIAware() -> BOOL;
}

// ============================================================================
// gdi32.dll
// ============================================================================

#[link(name = "gdi32")]
unsafe extern "system" {
    /// 创建实心画刷
    pub fn CreateSolidBrush(color: COLORREF) -> HBRUSH;

    /// 填充矩形
    pub fn FillRect(hDC: HDC, lprc: *const RECT, hbr: HBRUSH) -> i32;

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

    /// 创建 DIB 选区
    pub fn CreateDIBSection(
        hdc: HDC,
        lpbmih: *const BITMAPINFO,
        usage: UINT,
        ppvBits: *mut *mut c_void,
        hSection: isize,
        offset: DWORD,
    ) -> HBITMAP;

    /// 创建设备上下文 DC
    ///
    /// `CreateDCW("DISPLAY", null, null, null)` 获取整个显示设备的 DC，
    /// 区别于 `GetDC(NULL)` 返回的桌面窗口 DC（RDP 下可能为虚拟化 DC）。
    pub fn CreateDCW(
        lpszDriver: *const u16,
        lpszDevice: *const u16,
        lpszOutput: *const u16,
        lpInitData: *const c_void,
    ) -> HDC;
}

// ============================================================================
// 格式化消息常量
// ============================================================================

pub const FORMAT_MESSAGE_ALLOCATE_BUFFER: DWORD = 0x00000100;
pub const FORMAT_MESSAGE_FROM_SYSTEM: DWORD = 0x00001000;
pub const FORMAT_MESSAGE_IGNORE_INSERTS: DWORD = 0x00000200;
pub const LANG_NEUTRAL: DWORD = 0x00;
pub const SUBLANG_DEFAULT: DWORD = 0x01;

/// 组合语言 ID
pub const fn MAKELANGID(primary: DWORD, sub: DWORD) -> DWORD {
    (sub << 10) | primary
}
