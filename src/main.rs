#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(not(windows))]
compile_error!("hdr-auto is a Windows-only tray app.");

#[cfg(windows)]
mod app {
    use std::{
        collections::HashSet,
        ffi::{c_void, OsStr},
        fs, io, mem,
        os::windows::ffi::OsStrExt,
        path::{Path, PathBuf},
        process::Command,
        ptr,
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc, OnceLock,
        },
        thread,
        time::Duration,
    };

    use winapi::{
        shared::{
            minwindef::{DWORD, LPARAM, LRESULT, TRUE, UINT, WPARAM},
            ntdef::HANDLE,
            windef::{
                DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, HBRUSH, HCURSOR, HICON, HWND, POINT,
            },
            winerror::{ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_SUCCESS},
        },
        um::{
            errhandlingapi::{GetLastError, SetLastError},
            handleapi::{CloseHandle, INVALID_HANDLE_VALUE},
            libloaderapi::GetModuleHandleW,
            shellapi::{
                Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
                NOTIFYICONDATAW,
            },
            synchapi::CreateMutexW,
            tlhelp32::{
                CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
                TH32CS_SNAPPROCESS,
            },
            unknwnbase::IUnknown,
            winnt::{KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ},
            winreg::{
                RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW,
                RegSetValueExW, HKEY_CURRENT_USER,
            },
            winuser::{
                AppendMenuW, CopyImage, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
                DestroyIcon, DestroyMenu, DestroyWindow, DispatchMessageW, GetCursorPos,
                GetMessageW, GetSystemMetrics, LoadIconW, PostMessageW, PostQuitMessage,
                RegisterClassW, SendInput, SetForegroundWindow, SetProcessDPIAware,
                SetProcessDpiAwarenessContext, TrackPopupMenu, TranslateMessage, CS_HREDRAW,
                CS_VREDRAW, IDI_APPLICATION, IMAGE_ICON, INPUT, INPUT_KEYBOARD, KEYBDINPUT,
                KEYEVENTF_KEYUP, MF_CHECKED, MF_SEPARATOR, MF_STRING, MF_UNCHECKED, MSG,
                SM_CXSMICON, SM_CYSMICON, TPM_RIGHTBUTTON, VK_LWIN, VK_MENU, WM_APP, WM_CLOSE,
                WM_COMMAND, WM_DESTROY, WM_LBUTTONDBLCLK, WM_NULL, WM_RBUTTONUP, WNDCLASSW,
            },
        },
    };

    const APP_NAME: &str = "HDR Auto";
    const CLASS_NAME: &str = "HdrAutoTrayWindow";
    const SINGLE_INSTANCE_MUTEX: &str = "Local\\HdrAutoSingleInstance";
    const TRAY_UID: UINT = 1;
    const WM_TRAY_ICON: UINT = WM_APP + 1;
    const MENU_TOGGLE_HDR: usize = 1001;
    const MENU_USE_DEFAULT_LIST: usize = 1002;
    const MENU_USE_CUSTOM_LIST: usize = 1003;
    const MENU_EDIT_CUSTOM_LIST: usize = 1004;
    const MENU_RUN_AT_STARTUP: usize = 1005;
    const MENU_QUIT: usize = 1006;
    const POLL_INTERVAL: Duration = Duration::from_secs(1);
    const STARTUP_REGISTRY_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const STARTUP_REGISTRY_VALUE_NAME: &str = APP_NAME;
    const GAME_LIST_DEFAULT_FLAG: usize = 0b01;
    const GAME_LIST_CUSTOM_FLAG: usize = 0b10;
    const INITIAL_GAME_LIST_FLAGS: usize = GAME_LIST_DEFAULT_FLAG;

    static QUIT_FLAG: OnceLock<Arc<AtomicBool>> = OnceLock::new();
    static ACTIVE_GAME_LIST_FLAGS: OnceLock<Arc<AtomicUsize>> = OnceLock::new();
    static TRAY_ICON_HANDLE: AtomicUsize = AtomicUsize::new(0);

    const ICON_FILE_NAME: &str = "icon_tray.png";
    const GDIP_OK: i32 = 0;
    static EMBEDDED_ICON_PNG: &[u8] = include_bytes!("../icon_tray.png");

    enum GpBitmap {}
    enum GpImage {}

    #[repr(C)]
    struct GdiplusStartupInput {
        gdiplus_version: u32,
        debug_event_callback: *mut c_void,
        suppress_background_thread: i32,
        suppress_external_codecs: i32,
    }

    #[link(name = "gdiplus")]
    extern "system" {
        fn GdiplusStartup(
            token: *mut usize,
            input: *const GdiplusStartupInput,
            output: *mut c_void,
        ) -> i32;
        fn GdiplusShutdown(token: usize);
        fn GdipCreateBitmapFromFile(filename: *const u16, bitmap: *mut *mut GpBitmap) -> i32;
        fn GdipCreateBitmapFromStream(stream: *mut IUnknown, bitmap: *mut *mut GpBitmap) -> i32;
        fn GdipCreateHICONFromBitmap(bitmap: *mut GpBitmap, icon: *mut HICON) -> i32;
        fn GdipDisposeImage(image: *mut GpImage) -> i32;
    }

    #[link(name = "shlwapi")]
    extern "system" {
        fn SHCreateMemStream(init: *const u8, init_len: UINT) -> *mut IUnknown;
    }

    pub fn main() -> io::Result<()> {
        unsafe {
            enable_high_dpi_rendering();
        }

        let _single_instance = match SingleInstance::acquire(SINGLE_INSTANCE_MUTEX)? {
            Some(instance) => instance,
            None => return Ok(()),
        };

        let game_list_paths = game_list_paths()?;
        ensure_game_list_files(&game_list_paths)?;

        let quit = Arc::new(AtomicBool::new(false));
        let active_game_list_flags = Arc::new(AtomicUsize::new(INITIAL_GAME_LIST_FLAGS));
        let monitor_quit = Arc::clone(&quit);
        let monitor_game_list_paths = game_list_paths.clone();
        let monitor_game_list_flags = Arc::clone(&active_game_list_flags);
        let _ = QUIT_FLAG.set(Arc::clone(&quit));
        let _ = ACTIVE_GAME_LIST_FLAGS.set(Arc::clone(&active_game_list_flags));

        let monitor = thread::spawn(move || {
            monitor_games(
                monitor_game_list_paths,
                monitor_game_list_flags,
                monitor_quit,
            )
        });

        let tray_result = unsafe { run_tray_app() };
        quit.store(true, Ordering::SeqCst);
        let _ = monitor.join();

        tray_result
    }

    unsafe fn enable_high_dpi_rendering() {
        if SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) == 0 {
            SetProcessDPIAware();
        }
    }

    struct SingleInstance {
        handle: winapi::shared::ntdef::HANDLE,
    }

    impl SingleInstance {
        fn acquire(name: &str) -> io::Result<Option<Self>> {
            let name = to_wide_null(name);
            unsafe {
                SetLastError(0);
            }

            let handle = unsafe { CreateMutexW(ptr::null_mut(), TRUE, name.as_ptr()) };
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }

            let last_error = unsafe { GetLastError() };
            if last_error == ERROR_ALREADY_EXISTS {
                unsafe {
                    CloseHandle(handle);
                }
                return Ok(None);
            }

            Ok(Some(Self { handle }))
        }
    }

    impl Drop for SingleInstance {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }

    #[derive(Clone)]
    struct GameListPaths {
        default: PathBuf,
        custom: PathBuf,
    }

    fn game_list_paths() -> io::Result<GameListPaths> {
        let base_dir = game_list_base_dir()?;
        Ok(GameListPaths {
            default: base_dir.join("games_default.txt"),
            custom: base_dir.join("games_custom.txt"),
        })
    }

    fn game_list_base_dir() -> io::Result<PathBuf> {
        let exe = std::env::current_exe()?;
        let exe_dir = exe.parent().unwrap_or_else(|| Path::new("."));
        if has_game_list_file(exe_dir) {
            return Ok(exe_dir.to_path_buf());
        }

        let cwd = std::env::current_dir()?;
        if has_game_list_file(&cwd) {
            return Ok(cwd);
        }

        Ok(exe_dir.to_path_buf())
    }

    fn has_game_list_file(dir: &Path) -> bool {
        ["games_default.txt", "games_custom.txt"]
            .iter()
            .any(|name| dir.join(name).exists())
    }

    fn ensure_game_list_files(paths: &GameListPaths) -> io::Result<()> {
        if let Some(parent) = paths.default.parent() {
            fs::create_dir_all(parent)?;
        }

        if !paths.default.exists() {
            fs::write(
                &paths.default,
                concat!(
                    "# One process executable per line. Extension is optional.\n",
                    "# eldenring.exe\n",
                    "# Cyberpunk2077.exe\n",
                    "# bg3.exe\n"
                ),
            )?;
        }

        if !paths.custom.exists() {
            fs::write(&paths.custom, "")?;
        }

        Ok(())
    }

    fn monitor_games(
        game_list_paths: GameListPaths,
        active_game_list_flags: Arc<AtomicUsize>,
        quit: Arc<AtomicBool>,
    ) {
        let mut was_running = false;
        let mut initialized = false;
        let mut hdr_enabled_by_us = false;

        while !quit.load(Ordering::SeqCst) {
            let game_list_flags = active_game_list_flags.load(Ordering::SeqCst);
            let games = load_game_list(&game_list_paths, game_list_flags);
            let is_running = match matching_game_processes(&games) {
                Ok(matches) => !matches.is_empty(),
                Err(_) => {
                    sleep_until_next_poll(&quit);
                    continue;
                }
            };

            if !initialized {
                initialized = true;
            } else if is_running && !was_running {
                if send_win_alt_b().is_ok() {
                    hdr_enabled_by_us = true;
                }
            } else if !is_running && was_running {
                if hdr_enabled_by_us && send_win_alt_b().is_ok() {
                    hdr_enabled_by_us = false;
                }
            }

            was_running = is_running;
            sleep_until_next_poll(&quit);
        }
    }

    fn sleep_until_next_poll(quit: &AtomicBool) {
        let mut slept = Duration::ZERO;
        while slept < POLL_INTERVAL && !quit.load(Ordering::SeqCst) {
            let step = Duration::from_millis(100);
            thread::sleep(step);
            slept += step;
        }
    }

    fn load_game_list(paths: &GameListPaths, flags: usize) -> Vec<String> {
        let mut games = Vec::new();
        let mut seen = HashSet::new();
        if flags & GAME_LIST_DEFAULT_FLAG != 0 {
            append_game_list(&mut games, &mut seen, &paths.default);
        }
        if flags & GAME_LIST_CUSTOM_FLAG != 0 {
            append_game_list(&mut games, &mut seen, &paths.custom);
        }
        games
    }

    fn append_game_list(games: &mut Vec<String>, seen: &mut HashSet<String>, path: &Path) {
        if let Ok(contents) = fs::read_to_string(path) {
            for game in contents.lines().filter_map(normalize_game_name) {
                if seen.insert(game.clone()) {
                    games.push(game);
                }
            }
        }
    }

    fn normalize_game_name(line: &str) -> Option<String> {
        let name = line.trim().trim_matches('"');
        if name.is_empty() || name.starts_with('#') {
            return None;
        }

        let lower = name.to_ascii_lowercase();
        Some(lower.strip_suffix(".exe").unwrap_or(&lower).to_string())
    }

    fn matching_game_processes(game_names: &[String]) -> io::Result<HashSet<String>> {
        let mut matches = HashSet::new();
        if game_names.is_empty() {
            return Ok(matches);
        }

        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let _snapshot = SnapshotHandle(snapshot);

        let mut entry = unsafe { mem::zeroed::<PROCESSENTRY32W>() };
        entry.dwSize = mem::size_of::<PROCESSENTRY32W>() as DWORD;

        if unsafe { Process32FirstW(snapshot, &mut entry) } == 0 {
            return Ok(matches);
        }

        loop {
            let exe_name = fixed_wide_to_string(&entry.szExeFile);
            if process_matches(&exe_name, game_names) {
                matches.insert(exe_name);
            }

            if unsafe { Process32NextW(snapshot, &mut entry) } == 0 {
                break;
            }
        }

        Ok(matches)
    }

    struct SnapshotHandle(winapi::shared::ntdef::HANDLE);

    impl Drop for SnapshotHandle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    fn process_matches(exe_name: &str, game_names: &[String]) -> bool {
        let exe_key = normalize_process_key(exe_name);
        game_names.iter().any(|game| {
            if game == &exe_key {
                return true;
            }

            let game_key = normalize_process_key(game);
            let can_prefix_match = game_key.len() >= 5;
            !game_key.is_empty()
                && (exe_key == game_key
                    || (can_prefix_match && exe_key.starts_with(&game_key))
                    || known_suffix_trim(&exe_key) == game_key)
        })
    }

    fn normalize_process_key(value: &str) -> String {
        let lower = value.trim().trim_matches('"').to_ascii_lowercase();
        lower
            .strip_suffix(".exe")
            .unwrap_or(&lower)
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .flat_map(|c| c.to_lowercase())
            .collect()
    }

    fn known_suffix_trim(value: &str) -> &str {
        for suffix in ["win64shipping", "x64", "dx12", "dx11", "shipping"] {
            if let Some(trimmed) = value.strip_suffix(suffix) {
                return trimmed;
            }
        }

        value
    }

    fn send_win_alt_b() -> io::Result<()> {
        let mut inputs = [
            keyboard_input(VK_LWIN as u16, false),
            keyboard_input(VK_MENU as u16, false),
            keyboard_input(b'B' as u16, false),
            keyboard_input(b'B' as u16, true),
            keyboard_input(VK_MENU as u16, true),
            keyboard_input(VK_LWIN as u16, true),
        ];

        let sent = unsafe {
            SendInput(
                inputs.len() as UINT,
                inputs.as_mut_ptr(),
                mem::size_of::<INPUT>() as i32,
            )
        };

        if sent == inputs.len() as UINT {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn keyboard_input(vk: u16, key_up: bool) -> INPUT {
        let mut input = unsafe { mem::zeroed::<INPUT>() };
        input.type_ = INPUT_KEYBOARD;
        unsafe {
            *input.u.ki_mut() = KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if key_up { KEYEVENTF_KEYUP } else { 0 },
                time: 0,
                dwExtraInfo: 0,
            };
        }
        input
    }

    unsafe fn run_tray_app() -> io::Result<()> {
        let class_name = to_wide_null(CLASS_NAME);
        let window_name = to_wide_null(APP_NAME);
        let h_instance = GetModuleHandleW(ptr::null());
        if h_instance.is_null() {
            return Err(io::Error::last_os_error());
        }

        let window_class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(window_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: h_instance,
            hIcon: ptr::null_mut() as HICON,
            hCursor: ptr::null_mut() as HCURSOR,
            hbrBackground: ptr::null_mut() as HBRUSH,
            lpszMenuName: ptr::null(),
            lpszClassName: class_name.as_ptr(),
        };

        if RegisterClassW(&window_class) == 0 {
            return Err(io::Error::last_os_error());
        }

        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            window_name.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            h_instance,
            ptr::null_mut(),
        );

        if hwnd.is_null() {
            return Err(io::Error::last_os_error());
        }

        add_tray_icon(hwnd)?;
        message_loop()
    }

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        msg: UINT,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_TRAY_ICON => {
                match lparam as UINT {
                    WM_RBUTTONUP => show_context_menu(hwnd),
                    WM_LBUTTONDBLCLK => {
                        let _ = send_win_alt_b();
                    }
                    _ => {}
                }
                0
            }
            WM_COMMAND => {
                match loword(wparam as usize) as usize {
                    MENU_TOGGLE_HDR => {
                        let _ = send_win_alt_b();
                    }
                    MENU_USE_DEFAULT_LIST => {
                        toggle_game_list_flag(GAME_LIST_DEFAULT_FLAG);
                    }
                    MENU_USE_CUSTOM_LIST => {
                        toggle_game_list_flag(GAME_LIST_CUSTOM_FLAG);
                    }
                    MENU_EDIT_CUSTOM_LIST => {
                        let _ = edit_custom_game_list();
                    }
                    MENU_RUN_AT_STARTUP => {
                        let _ = set_startup_enabled(!startup_enabled());
                    }
                    MENU_QUIT => {
                        if let Some(quit) = QUIT_FLAG.get() {
                            quit.store(true, Ordering::SeqCst);
                        }
                        DestroyWindow(hwnd);
                    }
                    _ => {}
                }
                0
            }
            WM_CLOSE => {
                if let Some(quit) = QUIT_FLAG.get() {
                    quit.store(true, Ordering::SeqCst);
                }
                DestroyWindow(hwnd);
                0
            }
            WM_DESTROY => {
                remove_tray_icon(hwnd);
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    unsafe fn show_context_menu(hwnd: HWND) {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }

        let toggle = to_wide_null("Toggle HDR now");
        let default_list = to_wide_null("Use default game list");
        let custom_list = to_wide_null("Use custom game list");
        let edit_custom_list = to_wide_null("Edit custom game list");
        let startup = to_wide_null("Run at Windows startup");
        let quit = to_wide_null("Quit");
        let current_game_list_flags = game_list_flags();
        let run_at_startup = startup_enabled();
        AppendMenuW(menu, MF_STRING, MENU_TOGGLE_HDR, toggle.as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
        AppendMenuW(
            menu,
            MF_STRING | checked_if(current_game_list_flags & GAME_LIST_DEFAULT_FLAG != 0),
            MENU_USE_DEFAULT_LIST,
            default_list.as_ptr(),
        );
        AppendMenuW(
            menu,
            MF_STRING | checked_if(current_game_list_flags & GAME_LIST_CUSTOM_FLAG != 0),
            MENU_USE_CUSTOM_LIST,
            custom_list.as_ptr(),
        );
        AppendMenuW(
            menu,
            MF_STRING,
            MENU_EDIT_CUSTOM_LIST,
            edit_custom_list.as_ptr(),
        );
        AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
        AppendMenuW(
            menu,
            MF_STRING | checked_if(run_at_startup),
            MENU_RUN_AT_STARTUP,
            startup.as_ptr(),
        );
        AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
        AppendMenuW(menu, MF_STRING, MENU_QUIT, quit.as_ptr());

        let mut point = POINT { x: 0, y: 0 };
        if GetCursorPos(&mut point) != 0 {
            SetForegroundWindow(hwnd);
            TrackPopupMenu(
                menu,
                TPM_RIGHTBUTTON,
                point.x,
                point.y,
                0,
                hwnd,
                ptr::null(),
            );
            PostMessageW(hwnd, WM_NULL, 0, 0);
        }

        DestroyMenu(menu);
    }

    fn game_list_flags() -> usize {
        ACTIVE_GAME_LIST_FLAGS
            .get()
            .map(|flags| flags.load(Ordering::SeqCst))
            .unwrap_or(INITIAL_GAME_LIST_FLAGS)
    }

    fn toggle_game_list_flag(flag: usize) {
        if let Some(active_flags) = ACTIVE_GAME_LIST_FLAGS.get() {
            active_flags.fetch_xor(flag, Ordering::SeqCst);
        }
    }

    fn edit_custom_game_list() -> io::Result<()> {
        let paths = game_list_paths()?;
        ensure_game_list_files(&paths)?;
        Command::new("notepad.exe").arg(&paths.custom).spawn()?;
        Ok(())
    }

    fn startup_enabled() -> bool {
        let current_exe = match std::env::current_exe() {
            Ok(path) => path,
            Err(_) => return false,
        };

        match startup_command() {
            Ok(Some(command)) => command_exe_path(&command)
                .map(|path| same_path(&path, &current_exe))
                .unwrap_or(false),
            _ => false,
        }
    }

    fn set_startup_enabled(enabled: bool) -> io::Result<()> {
        if enabled {
            enable_startup()
        } else {
            disable_startup()
        }
    }

    fn enable_startup() -> io::Result<()> {
        let key = create_startup_key(KEY_SET_VALUE)?;
        let name = to_wide_null(STARTUP_REGISTRY_VALUE_NAME);
        let command = startup_command_wide(&std::env::current_exe()?);
        let byte_len = (command.len() * mem::size_of::<u16>()) as DWORD;
        let status = unsafe {
            RegSetValueExW(
                key.0,
                name.as_ptr(),
                0,
                REG_SZ,
                command.as_ptr() as *const u8,
                byte_len,
            )
        };

        if registry_status_is(status, ERROR_SUCCESS) {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(status as i32))
        }
    }

    fn disable_startup() -> io::Result<()> {
        let key = match open_startup_key(KEY_SET_VALUE) {
            Ok(key) => key,
            Err(error) if is_not_found(&error) => return Ok(()),
            Err(error) => return Err(error),
        };
        let name = to_wide_null(STARTUP_REGISTRY_VALUE_NAME);
        let status = unsafe { RegDeleteValueW(key.0, name.as_ptr()) };

        if registry_status_is(status, ERROR_SUCCESS)
            || registry_status_is(status, ERROR_FILE_NOT_FOUND)
        {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(status as i32))
        }
    }

    fn startup_command() -> io::Result<Option<String>> {
        let key = match open_startup_key(KEY_QUERY_VALUE) {
            Ok(key) => key,
            Err(error) if is_not_found(&error) => return Ok(None),
            Err(error) => return Err(error),
        };
        let name = to_wide_null(STARTUP_REGISTRY_VALUE_NAME);
        let mut value_type = 0;
        let mut byte_len = 0;
        let status = unsafe {
            RegQueryValueExW(
                key.0,
                name.as_ptr(),
                ptr::null_mut(),
                &mut value_type,
                ptr::null_mut(),
                &mut byte_len,
            )
        };

        if registry_status_is(status, ERROR_FILE_NOT_FOUND) {
            return Ok(None);
        }
        if !registry_status_is(status, ERROR_SUCCESS) {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        if value_type != REG_SZ || byte_len == 0 {
            return Ok(None);
        }

        let mut buffer = vec![0u16; (byte_len as usize + 1) / mem::size_of::<u16>()];
        let status = unsafe {
            RegQueryValueExW(
                key.0,
                name.as_ptr(),
                ptr::null_mut(),
                &mut value_type,
                buffer.as_mut_ptr() as *mut u8,
                &mut byte_len,
            )
        };

        if registry_status_is(status, ERROR_FILE_NOT_FOUND) {
            return Ok(None);
        }
        if !registry_status_is(status, ERROR_SUCCESS) {
            return Err(io::Error::from_raw_os_error(status as i32));
        }

        let char_len = (byte_len as usize / mem::size_of::<u16>()).min(buffer.len());
        buffer.truncate(char_len);
        if let Some(null_index) = buffer.iter().position(|&value| value == 0) {
            buffer.truncate(null_index);
        }

        Ok(Some(String::from_utf16_lossy(&buffer)))
    }

    struct RegistryKey(winapi::shared::minwindef::HKEY);

    impl Drop for RegistryKey {
        fn drop(&mut self) {
            unsafe {
                RegCloseKey(self.0);
            }
        }
    }

    fn open_startup_key(access: DWORD) -> io::Result<RegistryKey> {
        let subkey = to_wide_null(STARTUP_REGISTRY_SUBKEY);
        let mut key = ptr::null_mut();
        let status =
            unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, access, &mut key) };

        if registry_status_is(status, ERROR_SUCCESS) {
            Ok(RegistryKey(key))
        } else {
            Err(io::Error::from_raw_os_error(status as i32))
        }
    }

    fn create_startup_key(access: DWORD) -> io::Result<RegistryKey> {
        let subkey = to_wide_null(STARTUP_REGISTRY_SUBKEY);
        let mut key = ptr::null_mut();
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                subkey.as_ptr(),
                0,
                ptr::null_mut(),
                0,
                access,
                ptr::null_mut(),
                &mut key,
                ptr::null_mut(),
            )
        };

        if registry_status_is(status, ERROR_SUCCESS) {
            Ok(RegistryKey(key))
        } else {
            Err(io::Error::from_raw_os_error(status as i32))
        }
    }

    fn startup_command_wide(exe_path: &Path) -> Vec<u16> {
        let mut command = Vec::new();
        command.push('"' as u16);
        command.extend(exe_path.as_os_str().encode_wide());
        command.push('"' as u16);
        command.push(0);
        command
    }

    fn command_exe_path(command: &str) -> Option<PathBuf> {
        let trimmed = command.trim();
        if trimmed.is_empty() {
            return None;
        }

        if let Some(rest) = trimmed.strip_prefix('"') {
            let end_quote = rest.find('"')?;
            return Some(PathBuf::from(&rest[..end_quote]));
        }

        trimmed.split_whitespace().next().map(PathBuf::from)
    }

    fn same_path(left: &Path, right: &Path) -> bool {
        let left = left.canonicalize().unwrap_or_else(|_| left.to_path_buf());
        let right = right.canonicalize().unwrap_or_else(|_| right.to_path_buf());
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    }

    fn registry_status_is(status: i32, code: DWORD) -> bool {
        status == code as i32
    }

    fn is_not_found(error: &io::Error) -> bool {
        error.raw_os_error() == Some(ERROR_FILE_NOT_FOUND as i32)
    }

    fn checked_if(condition: bool) -> UINT {
        if condition {
            MF_CHECKED
        } else {
            MF_UNCHECKED
        }
    }

    unsafe fn add_tray_icon(hwnd: HWND) -> io::Result<()> {
        let mut data = tray_icon_data(hwnd);
        let tray_icon = load_tray_icon();
        data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        data.uCallbackMessage = WM_TRAY_ICON;
        data.hIcon = tray_icon.icon;
        copy_to_fixed_wide(&mut data.szTip, APP_NAME);

        if Shell_NotifyIconW(NIM_ADD, &mut data) == 0 {
            if tray_icon.owned {
                DestroyIcon(tray_icon.icon);
            }
            Err(io::Error::last_os_error())
        } else {
            if tray_icon.owned {
                TRAY_ICON_HANDLE.store(tray_icon.icon as usize, Ordering::SeqCst);
            }
            Ok(())
        }
    }

    unsafe fn remove_tray_icon(hwnd: HWND) {
        let mut data = tray_icon_data(hwnd);
        Shell_NotifyIconW(NIM_DELETE, &mut data);
        let icon = TRAY_ICON_HANDLE.swap(0, Ordering::SeqCst);
        if icon != 0 {
            DestroyIcon(icon as HICON);
        }
    }

    struct TrayIcon {
        icon: HICON,
        owned: bool,
    }

    unsafe fn load_tray_icon() -> TrayIcon {
        if let Ok(icon) = load_embedded_png_icon() {
            return TrayIcon { icon, owned: true };
        }

        if let Some(path) = icon_path() {
            if let Ok(icon) = load_png_icon(&path) {
                return TrayIcon { icon, owned: true };
            }
        }

        TrayIcon {
            icon: LoadIconW(ptr::null_mut(), IDI_APPLICATION),
            owned: false,
        }
    }

    fn icon_path() -> Option<PathBuf> {
        let exe_dir_path = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join(ICON_FILE_NAME)));
        if let Some(path) = exe_dir_path {
            if path.exists() {
                return Some(path);
            }
        }

        let cwd_path = std::env::current_dir().ok()?.join(ICON_FILE_NAME);
        if cwd_path.exists() {
            Some(cwd_path)
        } else {
            None
        }
    }

    unsafe fn load_embedded_png_icon() -> io::Result<HICON> {
        let _gdiplus = GdiplusToken::start()?;
        let stream = SHCreateMemStream(EMBEDDED_ICON_PNG.as_ptr(), EMBEDDED_ICON_PNG.len() as UINT);
        if stream.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "SHCreateMemStream failed",
            ));
        }

        let _stream = ComStream(stream);
        let mut bitmap = ptr::null_mut();
        let status = GdipCreateBitmapFromStream(stream, &mut bitmap);
        if status != GDIP_OK || bitmap.is_null() {
            return Err(gdiplus_error("GdipCreateBitmapFromStream", status));
        }

        bitmap_to_icon(bitmap)
    }

    unsafe fn load_png_icon(path: &Path) -> io::Result<HICON> {
        let _gdiplus = GdiplusToken::start()?;
        let path = path
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let mut bitmap = ptr::null_mut();
        let status = GdipCreateBitmapFromFile(path.as_ptr(), &mut bitmap);
        if status != GDIP_OK || bitmap.is_null() {
            return Err(gdiplus_error("GdipCreateBitmapFromFile", status));
        }

        bitmap_to_icon(bitmap)
    }

    unsafe fn bitmap_to_icon(bitmap: *mut GpBitmap) -> io::Result<HICON> {
        let _bitmap = GdiplusImage(bitmap as *mut GpImage);
        let mut icon = ptr::null_mut();
        let status = GdipCreateHICONFromBitmap(bitmap, &mut icon);
        if status != GDIP_OK || icon.is_null() {
            return Err(gdiplus_error("GdipCreateHICONFromBitmap", status));
        }

        Ok(scale_icon_for_tray(icon))
    }

    unsafe fn scale_icon_for_tray(icon: HICON) -> HICON {
        let width = GetSystemMetrics(SM_CXSMICON);
        let height = GetSystemMetrics(SM_CYSMICON);
        if width <= 0 || height <= 0 {
            return icon;
        }

        let scaled = CopyImage(icon as HANDLE, IMAGE_ICON, width, height, 0) as HICON;
        if scaled.is_null() {
            icon
        } else {
            DestroyIcon(icon);
            scaled
        }
    }

    struct ComStream(*mut IUnknown);

    impl Drop for ComStream {
        fn drop(&mut self) {
            unsafe {
                (*self.0).Release();
            }
        }
    }

    struct GdiplusToken(usize);

    impl GdiplusToken {
        unsafe fn start() -> io::Result<Self> {
            let input = GdiplusStartupInput {
                gdiplus_version: 1,
                debug_event_callback: ptr::null_mut(),
                suppress_background_thread: 0,
                suppress_external_codecs: 0,
            };
            let mut token = 0;
            let status = GdiplusStartup(&mut token, &input, ptr::null_mut());
            if status == GDIP_OK {
                Ok(Self(token))
            } else {
                Err(gdiplus_error("GdiplusStartup", status))
            }
        }
    }

    impl Drop for GdiplusToken {
        fn drop(&mut self) {
            unsafe {
                GdiplusShutdown(self.0);
            }
        }
    }

    struct GdiplusImage(*mut GpImage);

    impl Drop for GdiplusImage {
        fn drop(&mut self) {
            unsafe {
                GdipDisposeImage(self.0);
            }
        }
    }

    fn gdiplus_error(operation: &str, status: i32) -> io::Error {
        io::Error::new(
            io::ErrorKind::Other,
            format!("{operation} failed with GDI+ status {status}"),
        )
    }

    fn tray_icon_data(hwnd: HWND) -> NOTIFYICONDATAW {
        let mut data = unsafe { mem::zeroed::<NOTIFYICONDATAW>() };
        data.cbSize = mem::size_of::<NOTIFYICONDATAW>() as DWORD;
        data.hWnd = hwnd;
        data.uID = TRAY_UID;
        data
    }

    unsafe fn message_loop() -> io::Result<()> {
        let mut msg = mem::zeroed::<MSG>();

        loop {
            let status = GetMessageW(&mut msg, ptr::null_mut(), 0, 0);
            if status == -1 {
                return Err(io::Error::last_os_error());
            }
            if status == 0 {
                break;
            }

            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        Ok(())
    }

    fn loword(value: usize) -> u16 {
        (value & 0xffff) as u16
    }

    fn to_wide_null(value: &str) -> Vec<u16> {
        OsStr::new(value).encode_wide().chain(Some(0)).collect()
    }

    fn fixed_wide_to_string(value: &[u16]) -> String {
        let len = value.iter().position(|&c| c == 0).unwrap_or(value.len());
        String::from_utf16_lossy(&value[..len])
    }

    fn copy_to_fixed_wide(target: &mut [u16], value: &str) {
        if target.is_empty() {
            return;
        }

        let wide = to_wide_null(value);
        let copy_len = wide.len().min(target.len());
        target[..copy_len].copy_from_slice(&wide[..copy_len]);
        target[target.len() - 1] = 0;
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::time::{SystemTime, UNIX_EPOCH};

        #[test]
        fn load_game_list_deduplicates_across_enabled_lists() -> io::Result<()> {
            let dir = unique_temp_dir("dedupe");
            fs::create_dir_all(&dir)?;
            let paths = GameListPaths {
                default: dir.join("games_default.txt"),
                custom: dir.join("games_custom.txt"),
            };

            fs::write(&paths.default, "Game.exe\n\"Other Game.exe\"\n")?;
            fs::write(&paths.custom, "game\nthird.exe\nother game.exe\n")?;

            let games = load_game_list(&paths, GAME_LIST_DEFAULT_FLAG | GAME_LIST_CUSTOM_FLAG);

            fs::remove_dir_all(&dir)?;
            assert_eq!(
                games,
                vec![
                    "game".to_string(),
                    "other game".to_string(),
                    "third".to_string()
                ]
            );
            Ok(())
        }

        #[test]
        fn load_game_list_returns_empty_when_no_lists_enabled() -> io::Result<()> {
            let dir = unique_temp_dir("empty");
            fs::create_dir_all(&dir)?;
            let paths = GameListPaths {
                default: dir.join("games_default.txt"),
                custom: dir.join("games_custom.txt"),
            };

            fs::write(&paths.default, "game.exe\n")?;
            fs::write(&paths.custom, "other.exe\n")?;

            let games = load_game_list(&paths, 0);

            fs::remove_dir_all(&dir)?;
            assert!(games.is_empty());
            Ok(())
        }

        fn unique_temp_dir(name: &str) -> PathBuf {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock should be after UNIX_EPOCH")
                .as_nanos();
            std::env::temp_dir().join(format!("hdr-auto-{name}-{nanos}"))
        }
    }
}

#[cfg(windows)]
fn main() -> std::io::Result<()> {
    app::main()
}
