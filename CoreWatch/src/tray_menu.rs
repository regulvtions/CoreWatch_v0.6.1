//! Native owner-drawn popup: Windows supplies hit testing, keyboard navigation,
//! selection and dismissal; we supply dark rows and small line icons.
use windows_sys::Win32::{Foundation::*, Graphics::Gdi::*, System::LibraryLoader::GetModuleHandleW, UI::WindowsAndMessaging::*};
use windows_sys::Win32::UI::{Controls::{DRAWITEMSTRUCT,MEASUREITEMSTRUCT,ODS_SELECTED},HiDpi::GetDpiForWindow};
const OPEN:usize=1; const QUIT:usize=2; const SEPARATOR:usize=3;
const BG:u32=0x002B2B2B; const HOVER:u32=0x003C3C3C; const TEXT:u32=0x00EEEEEE;
#[derive(Clone,Copy)]
pub struct MenuHandle { owner:isize, menu:isize }
pub struct TrayMenu { handle:MenuHandle, brush:HBRUSH }
impl TrayMenu {
    pub fn new()->Result<Self,String> { unsafe {
        let class=wide("CoreWatchTrayMenu");
        let instance=GetModuleHandleW(std::ptr::null());
        let wc=WNDCLASSW{lpfnWndProc:Some(window_proc),hInstance:instance,lpszClassName:class.as_ptr(),..std::mem::zeroed()};
        RegisterClassW(&wc);
        let owner=CreateWindowExW(WS_EX_TOOLWINDOW,class.as_ptr(),class.as_ptr(),WS_POPUP,0,0,0,0,std::ptr::null_mut(),std::ptr::null_mut(),instance,std::ptr::null());
        if owner.is_null(){return Err(std::io::Error::last_os_error().to_string());}
        let menu=CreatePopupMenu();
        if menu.is_null(){DestroyWindow(owner);return Err(std::io::Error::last_os_error().to_string());}
        let brush=CreateSolidBrush(BG);
        let popup=Self{handle:MenuHandle{owner:owner as isize,menu:menu as isize},brush};
        let info=MENUINFO{cbSize:std::mem::size_of::<MENUINFO>() as u32,fMask:MIM_BACKGROUND,hbrBack:brush,..std::mem::zeroed()};
        if brush.is_null() || SetMenuInfo(menu,&info)==0 || AppendMenuW(menu,MF_OWNERDRAW,OPEN,std::ptr::null())==0 || AppendMenuW(menu,MF_OWNERDRAW|MF_DISABLED,SEPARATOR,std::ptr::null())==0 || AppendMenuW(menu,MF_OWNERDRAW,QUIT,std::ptr::null())==0 {
            return Err("Could not create tray menu".into());
        }
        Ok(popup)
    }}
    pub fn handle(&self)->MenuHandle {self.handle}
}
impl Drop for TrayMenu {fn drop(&mut self){unsafe{
    DestroyMenu(self.handle.menu as HMENU);DestroyWindow(self.handle.owner as HWND);DeleteObject(self.brush);
}}}
impl MenuHandle {
    // Returns 1=Open, 2=Quit, 0=dismiss. Must run on the tray's UI thread.
    pub fn show(self)->u32 {unsafe{
        let hwnd=self.owner as HWND;
        let mut p:POINT=std::mem::zeroed();GetCursorPos(&mut p);
        // Position the owner on the active monitor so DPI sizing follows the popup.
        SetWindowPos(hwnd,std::ptr::null_mut(),p.x,p.y,0,0,SWP_NOACTIVATE|SWP_NOSIZE|SWP_NOZORDER);
        SetForegroundWindow(hwnd);
        let result=TrackPopupMenu(self.menu as HMENU,TPM_RETURNCMD|TPM_RIGHTBUTTON,p.x,p.y,0,hwnd,std::ptr::null());
        PostMessageW(hwnd,WM_NULL,0,0);
        result as u32
    }}
}
fn wide(s:&str)->Vec<u16>{s.encode_utf16().chain(Some(0)).collect()}
unsafe extern "system" fn window_proc(hwnd:HWND,msg:u32,w:WPARAM,l:LPARAM)->LRESULT {
    match msg {
        WM_MEASUREITEM=>{
            let item=&mut *(l as *mut MEASUREITEMSTRUCT);
            let dpi=GetDpiForWindow(hwnd).max(96);
            item.itemWidth=220*dpi/96;
            item.itemHeight=if item.itemID==SEPARATOR as u32{9*dpi/96}else{34*dpi/96};
            1
        }
        WM_DRAWITEM=>{
            let item=&*(l as *const DRAWITEMSTRUCT);
            let scale=GetDpiForWindow(hwnd).max(96) as f32/96.0;
            draw(item,scale);1
        }
        _=>DefWindowProcW(hwnd,msg,w,l)
    }
}
unsafe fn draw(item:&DRAWITEMSTRUCT,scale:f32) {
    let dc=item.hDC;let r=item.rcItem;let n=|v:f32|(v*scale).round() as i32;
    let selected=item.itemState & ODS_SELECTED !=0;
    let color=if selected{HOVER}else{BG};
    let brush=CreateSolidBrush(color);FillRect(dc,&r,brush);DeleteObject(brush);
    let pen=CreatePen(PS_SOLID,n(1.0).max(1),if item.itemID==SEPARATOR as u32{0x00454545}else{TEXT});
    let old_pen=SelectObject(dc,pen);
    if item.itemID==SEPARATOR as u32 {
        let y=(r.top+r.bottom)/2;MoveToEx(dc,r.left+n(10.),y,std::ptr::null_mut());LineTo(dc,r.right-n(10.),y);
    } else {
        let x=r.left+n(18.);let y=(r.top+r.bottom)/2;
        let old_brush=SelectObject(dc,GetStockObject(NULL_BRUSH));
        if item.itemID==OPEN as u32 {
            Rectangle(dc,x-n(6.),y-n(5.),x+n(7.),y+n(6.));
            MoveToEx(dc,x-n(6.),y-n(2.),std::ptr::null_mut());LineTo(dc,x+n(6.),y-n(2.));
        } else {
            Arc(dc,x-n(6.),y-n(6.),x+n(7.),y+n(7.),x-n(3.),y-n(6.),x+n(3.),y-n(6.));
            MoveToEx(dc,x,y-n(8.),std::ptr::null_mut());LineTo(dc,x,y);
        }
        SelectObject(dc,old_brush);
        let font=CreateFontW(-n(13.),0,0,0,FW_NORMAL as i32,0,0,0,DEFAULT_CHARSET as u32,0,0,CLEARTYPE_QUALITY as u32,0,wide("Segoe UI").as_ptr());
        let old_font=SelectObject(dc,font);SetBkMode(dc,TRANSPARENT as i32);SetTextColor(dc,TEXT);
        let label=wide(if item.itemID==OPEN as u32{"Open CoreWatch"}else{"Quit CoreWatch"});
        let mut text_rect=RECT{left:r.left+n(38.),right:r.right-n(12.),..r};
        DrawTextW(dc,label.as_ptr(),(label.len()-1) as i32,&mut text_rect,DT_SINGLELINE|DT_VCENTER|DT_LEFT);
        SelectObject(dc,old_font);DeleteObject(font);
    }
    SelectObject(dc,old_pen);DeleteObject(pen);
}
