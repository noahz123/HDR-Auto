#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(not(windows))]
compile_error!("hdr-auto is a Windows-only tray app.");

#[cfg(windows)]
mod app {
    use std::{
        collections::HashSet,
        ffi::OsStr,
        fs, io, mem,
        os::windows::ffi::OsStrExt,
        path::{Path, PathBuf},
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
            windef::{HBRUSH, HCURSOR, HICON, HWND, POINT},
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
            winnt::{KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ},
            winreg::{
                RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW,
                RegSetValueExW, HKEY_CURRENT_USER,
            },
            winuser::{
                AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
                DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, LoadIconW,
                PostMessageW, PostQuitMessage, RegisterClassW, SendInput, SetForegroundWindow,
                TrackPopupMenu, TranslateMessage, CS_HREDRAW, CS_VREDRAW, IDI_APPLICATION, INPUT,
                INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, MF_CHECKED, MF_SEPARATOR, MF_STRING,
                MF_UNCHECKED, MSG, TPM_RIGHTBUTTON, VK_LWIN, VK_MENU, WM_APP, WM_CLOSE, WM_COMMAND,
                WM_DESTROY, WM_LBUTTONDBLCLK, WM_NULL, WM_RBUTTONUP, WNDCLASSW,
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
    const MENU_RUN_AT_STARTUP: usize = 1004;
    const MENU_QUIT: usize = 1005;
    const POLL_INTERVAL: Duration = Duration::from_secs(1);
    const STARTUP_REGISTRY_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const STARTUP_REGISTRY_VALUE_NAME: &str = APP_NAME;

    static QUIT_FLAG: OnceLock<Arc<AtomicBool>> = OnceLock::new();
    static ACTIVE_GAME_LIST_MODE: OnceLock<Arc<AtomicUsize>> = OnceLock::new();

    pub fn main() -> io::Result<()> {
        let _single_instance = match SingleInstance::acquire(SINGLE_INSTANCE_MUTEX)? {
            Some(instance) => instance,
            None => return Ok(()),
        };

        let game_list_paths = game_list_paths()?;
        ensure_game_list_files(&game_list_paths)?;

        let quit = Arc::new(AtomicBool::new(false));
        let active_game_list_mode = Arc::new(AtomicUsize::new(GameListMode::Default as usize));
        let monitor_quit = Arc::clone(&quit);
        let monitor_game_list_paths = game_list_paths.clone();
        let monitor_game_list_mode = Arc::clone(&active_game_list_mode);
        let _ = QUIT_FLAG.set(Arc::clone(&quit));
        let _ = ACTIVE_GAME_LIST_MODE.set(Arc::clone(&active_game_list_mode));

        let monitor = thread::spawn(move || {
            monitor_games(
                monitor_game_list_paths,
                monitor_game_list_mode,
                monitor_quit,
            )
        });

        let tray_result = unsafe { run_tray_app() };
        quit.store(true, Ordering::SeqCst);
        let _ = monitor.join();

        tray_result
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

    #[derive(Clone, Copy, Eq, PartialEq)]
    enum GameListMode {
        Default = 0,
        Custom = 1,
    }

    impl GameListMode {
        fn from_usize(value: usize) -> Self {
            match value {
                value if value == Self::Custom as usize => Self::Custom,
                _ => Self::Default,
            }
        }
    }

    fn monitor_games(
        game_list_paths: GameListPaths,
        active_game_list_mode: Arc<AtomicUsize>,
        quit: Arc<AtomicBool>,
    ) {
        let mut was_running = false;
        let mut initialized = false;
        let mut hdr_enabled_by_us = false;

        while !quit.load(Ordering::SeqCst) {
            let game_list_mode =
                GameListMode::from_usize(active_game_list_mode.load(Ordering::SeqCst));
            let games = load_game_list(&game_list_paths, game_list_mode);
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

    fn load_game_list(paths: &GameListPaths, mode: GameListMode) -> Vec<String> {
        let mut games = Vec::new();
        let path = match mode {
            GameListMode::Default => &paths.default,
            GameListMode::Custom => &paths.custom,
        };
        append_game_list(&mut games, path);
        games
    }

    fn append_game_list(games: &mut Vec<String>, path: &Path) {
        if let Ok(contents) = fs::read_to_string(path) {
            games.extend(contents.lines().filter_map(normalize_game_name));
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
                        set_game_list_mode(GameListMode::Default);
                    }
                    MENU_USE_CUSTOM_LIST => {
                        set_game_list_mode(GameListMode::Custom);
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
        let startup = to_wide_null("Run at Windows startup");
        let quit = to_wide_null("Quit");
        let current_game_list_mode = game_list_mode();
        let run_at_startup = startup_enabled();
        AppendMenuW(menu, MF_STRING, MENU_TOGGLE_HDR, toggle.as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
        AppendMenuW(
            menu,
            MF_STRING | checked_if(current_game_list_mode == GameListMode::Default),
            MENU_USE_DEFAULT_LIST,
            default_list.as_ptr(),
        );
        AppendMenuW(
            menu,
            MF_STRING | checked_if(current_game_list_mode == GameListMode::Custom),
            MENU_USE_CUSTOM_LIST,
            custom_list.as_ptr(),
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

    fn game_list_mode() -> GameListMode {
        ACTIVE_GAME_LIST_MODE
            .get()
            .map(|mode| GameListMode::from_usize(mode.load(Ordering::SeqCst)))
            .unwrap_or(GameListMode::Default)
    }

    fn set_game_list_mode(mode: GameListMode) {
        if let Some(active_mode) = ACTIVE_GAME_LIST_MODE.get() {
            active_mode.store(mode as usize, Ordering::SeqCst);
        }
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
        data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        data.uCallbackMessage = WM_TRAY_ICON;
        data.hIcon = LoadIconW(ptr::null_mut(), IDI_APPLICATION);
        copy_to_fixed_wide(&mut data.szTip, APP_NAME);

        if Shell_NotifyIconW(NIM_ADD, &mut data) == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    unsafe fn remove_tray_icon(hwnd: HWND) {
        let mut data = tray_icon_data(hwnd);
        Shell_NotifyIconW(NIM_DELETE, &mut data);
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
}

#[cfg(windows)]
fn main() -> std::io::Result<()> {
    app::main()
}
