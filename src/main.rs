#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(not(windows))]
compile_error!("hdr-auto is a Windows-only tray app.");

#[cfg(windows)]
mod app {
    use std::{
        collections::HashSet,
        ffi::{c_void, OsStr, OsString},
        fs,
        io::{self, Read, Seek, SeekFrom},
        mem,
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
            minwindef::{DWORD, HKEY, LPARAM, LRESULT, TRUE, UINT, WPARAM},
            ntdef::HANDLE,
            windef::{
                DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, HBRUSH, HCURSOR, HICON, HWND, POINT,
            },
            winerror::{
                ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_INSUFFICIENT_BUFFER,
                ERROR_INVALID_PARAMETER, ERROR_NOT_SUPPORTED, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS,
            },
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
            wingdi::{
                DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
                DISPLAYCONFIG_DEVICE_INFO_HEADER,
                DISPLAYCONFIG_DEVICE_INFO_SET_ADVANCED_COLOR_STATE,
                DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO, DISPLAYCONFIG_MODE_INFO,
                DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE,
                DISPLAYCONFIG_TOPOLOGY_ID, QDC_ONLY_ACTIVE_PATHS,
            },
            winnt::{KEY_QUERY_VALUE, KEY_SET_VALUE, REG_DWORD, REG_SZ},
            winreg::{
                RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegDeleteValueW, RegOpenKeyExW,
                RegQueryValueExW, RegSetValueExW, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE,
            },
            winuser::{
                AppendMenuW, CopyImage, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
                DestroyIcon, DestroyMenu, DestroyWindow, DispatchMessageW, GetCursorPos,
                GetMessageW, GetSystemMetrics, LoadIconW, PostMessageW, PostQuitMessage,
                RegisterClassW, SetForegroundWindow, SetProcessDPIAware,
                SetProcessDpiAwarenessContext, TrackPopupMenu, TranslateMessage, CS_HREDRAW,
                CS_VREDRAW, IDI_APPLICATION, IMAGE_ICON, MF_CHECKED, MF_SEPARATOR, MF_STRING,
                MF_UNCHECKED, MSG, SM_CXSMICON, SM_CYSMICON, TPM_RIGHTBUTTON, WM_APP, WM_CLOSE,
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
    const MENU_VIDEO_CONTEXT_MENU: usize = 1006;
    const MENU_QUIT: usize = 1007;
    const POLL_INTERVAL: Duration = Duration::from_secs(1);
    const SETTINGS_REGISTRY_SUBKEY: &str = r"Software\HDR Auto";
    const GAME_LIST_FLAGS_REGISTRY_VALUE_NAME: &str = "GameListFlags";
    const STARTUP_REGISTRY_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const STARTUP_REGISTRY_VALUE_NAME: &str = APP_NAME;
    const APPLICATION_REGISTRY_SUBKEY: &str = r"Software\Classes\Applications\hdr-auto.exe";
    const VIDEO_CONTEXT_MENU_KEY_NAME: &str = "HdrAuto";
    const VIDEO_CONTEXT_MENU_TEXT: &str = "Open with HDR Auto";
    const ARG_OPEN_VIDEO: &str = "--open-video";
    const ARG_INSTALL_VIDEO_CONTEXT_MENU: &str = "--install-video-context-menu";
    const ARG_UNINSTALL_VIDEO_CONTEXT_MENU: &str = "--uninstall-video-context-menu";
    const DEFAULT_GAME_LIST_URL: &str =
        "https://raw.githubusercontent.com/noahz123/HDR-Auto/main/games_default.txt";
    const GAME_LIST_DEFAULT_FLAG: usize = 0b01;
    const GAME_LIST_CUSTOM_FLAG: usize = 0b10;
    const ALL_GAME_LIST_FLAGS: usize = GAME_LIST_DEFAULT_FLAG | GAME_LIST_CUSTOM_FLAG;
    const INITIAL_GAME_LIST_FLAGS: usize = GAME_LIST_DEFAULT_FLAG;
    const GAME_LIST_DOWNLOAD_TIMEOUT_MS: DWORD = 5_000;
    const HTTP_STATUS_OK: DWORD = 200;
    const INTERNET_OPEN_TYPE_PRECONFIG: DWORD = 0;
    const INTERNET_OPTION_CONNECT_TIMEOUT: DWORD = 2;
    const INTERNET_OPTION_SEND_TIMEOUT: DWORD = 5;
    const INTERNET_OPTION_RECEIVE_TIMEOUT: DWORD = 6;
    const INTERNET_FLAG_RELOAD: DWORD = 0x8000_0000;
    const INTERNET_FLAG_NO_CACHE_WRITE: DWORD = 0x0400_0000;
    const HTTP_QUERY_STATUS_CODE: DWORD = 19;
    const HTTP_QUERY_FLAG_NUMBER: DWORD = 0x2000_0000;
    const DOWNLOAD_BUFFER_SIZE: usize = 8 * 1024;
    const MIN_DOWNLOADED_GAME_LIST_ENTRIES: usize = 25;
    const MAX_VIDEO_METADATA_SCAN_BYTES: usize = 128 * 1024 * 1024;
    const MAX_TS_ELEMENTARY_STREAM_SCAN_BYTES: usize = 16 * 1024 * 1024;
    const HDR_COLOR_PRIMARIES_BT2020: u64 = 9;
    const HDR_TRANSFER_PQ: u64 = 16;
    const HDR_TRANSFER_HLG: u64 = 18;
    const HEVC_NAL_SPS: u8 = 33;
    const HEVC_NAL_PREFIX_SEI: u8 = 39;
    const HEVC_NAL_SUFFIX_SEI: u8 = 40;
    const HEVC_SEI_USER_DATA_REGISTERED_ITU_T_T35: u64 = 4;
    const HEVC_SEI_MASTERING_DISPLAY_COLOUR_VOLUME: u64 = 137;
    const HEVC_SEI_CONTENT_LIGHT_LEVEL_INFO: u64 = 144;
    const HEVC_SEI_ALTERNATIVE_TRANSFER_CHARACTERISTICS: u64 = 147;
    const MP4_BOX_HEADER_SIZE: usize = 8;
    const MP4_EXTENDED_BOX_HEADER_SIZE: usize = 16;
    const TS_PACKET_SIZE: usize = 188;
    const M2TS_PACKET_SIZE: usize = 192;
    const DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO_2_RAW: UINT = 15;
    const DISPLAYCONFIG_DEVICE_INFO_SET_HDR_STATE_RAW: UINT = 16;
    const DISPLAYCONFIG_HDR_SUPPORTED_MASK: UINT = 1 << 4;
    const DISPLAYCONFIG_HDR_USER_ENABLED_MASK: UINT = 1 << 5;
    const DISPLAYCONFIG_ENABLE_HDR_MASK: UINT = 1;
    const DISPLAYCONFIG_LEGACY_WIDE_COLOR_ENFORCED_MASK: UINT = 1 << 2;
    const DISPLAYCONFIG_LEGACY_ADVANCED_COLOR_FORCE_DISABLED_MASK: UINT = 1 << 3;
    const POTPLAYER_ENV_VAR: &str = "HDR_AUTO_POTPLAYER";
    const POTPLAYER_EXE_NAMES: &[&str] = &[
        "PotPlayer.exe",
        "PotPlayer64.exe",
        "PotPlayerMini64.exe",
        "PotPlayerMini.exe",
    ];
    const VIDEO_FILE_EXTENSIONS: &[&str] = &[
        ".3g2", ".3gp", ".avi", ".divx", ".flv", ".m2ts", ".m4v", ".mkv", ".mov", ".mp4", ".mpeg",
        ".mpg", ".mts", ".ogm", ".ogv", ".ts", ".vob", ".webm", ".wmv",
    ];

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

    #[link(name = "wininet")]
    extern "system" {
        fn InternetOpenW(
            agent: *const u16,
            access_type: DWORD,
            proxy: *const u16,
            proxy_bypass: *const u16,
            flags: DWORD,
        ) -> *mut c_void;
        fn InternetOpenUrlW(
            internet: *mut c_void,
            url: *const u16,
            headers: *const u16,
            headers_len: DWORD,
            flags: DWORD,
            context: usize,
        ) -> *mut c_void;
        fn InternetReadFile(
            file: *mut c_void,
            buffer: *mut c_void,
            bytes_to_read: DWORD,
            bytes_read: *mut DWORD,
        ) -> i32;
        fn InternetSetOptionW(
            internet: *mut c_void,
            option: DWORD,
            buffer: *mut c_void,
            buffer_len: DWORD,
        ) -> i32;
        fn InternetCloseHandle(internet: *mut c_void) -> i32;
        fn HttpQueryInfoW(
            request: *mut c_void,
            info_level: DWORD,
            buffer: *mut c_void,
            buffer_len: *mut DWORD,
            index: *mut DWORD,
        ) -> i32;
    }

    #[link(name = "user32")]
    extern "system" {
        fn GetDisplayConfigBufferSizes(
            flags: UINT,
            num_path_array_elements: *mut UINT,
            num_mode_info_array_elements: *mut UINT,
        ) -> i32;
        fn QueryDisplayConfig(
            flags: UINT,
            num_path_array_elements: *mut UINT,
            path_array: *mut DISPLAYCONFIG_PATH_INFO,
            num_mode_info_array_elements: *mut UINT,
            mode_info_array: *mut DISPLAYCONFIG_MODE_INFO,
            current_topology_id: *mut DISPLAYCONFIG_TOPOLOGY_ID,
        ) -> i32;
        fn DisplayConfigGetDeviceInfo(request_packet: *mut DISPLAYCONFIG_DEVICE_INFO_HEADER)
            -> i32;
        fn DisplayConfigSetDeviceInfo(request_packet: *mut DISPLAYCONFIG_DEVICE_INFO_HEADER)
            -> i32;
    }

    pub fn main() -> io::Result<()> {
        unsafe {
            enable_high_dpi_rendering();
        }

        match startup_mode(std::env::args_os().skip(1)) {
            StartupMode::Tray => {}
            StartupMode::OpenVideos(paths) => return open_videos_with_potplayer(&paths),
            StartupMode::InstallVideoContextMenu => return enable_video_context_menu(),
            StartupMode::UninstallVideoContextMenu => return disable_video_context_menu(),
        }

        let _single_instance = match SingleInstance::acquire(SINGLE_INSTANCE_MUTEX)? {
            Some(instance) => instance,
            None => return Ok(()),
        };

        let game_list_paths = game_list_paths()?;
        ensure_game_list_files(&game_list_paths)?;
        let _ = refresh_default_game_list(&game_list_paths);

        let quit = Arc::new(AtomicBool::new(false));
        let initial_game_list_flags =
            load_saved_game_list_flags().unwrap_or(INITIAL_GAME_LIST_FLAGS);
        let active_game_list_flags = Arc::new(AtomicUsize::new(initial_game_list_flags));
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

    enum StartupMode {
        Tray,
        OpenVideos(Vec<PathBuf>),
        InstallVideoContextMenu,
        UninstallVideoContextMenu,
    }

    fn startup_mode(args: impl IntoIterator<Item = OsString>) -> StartupMode {
        let mut args = args.into_iter();
        let Some(first) = args.next() else {
            return StartupMode::Tray;
        };

        if first == OsStr::new(ARG_INSTALL_VIDEO_CONTEXT_MENU) {
            return StartupMode::InstallVideoContextMenu;
        }
        if first == OsStr::new(ARG_UNINSTALL_VIDEO_CONTEXT_MENU) {
            return StartupMode::UninstallVideoContextMenu;
        }
        if first == OsStr::new(ARG_OPEN_VIDEO) {
            let paths = args.map(PathBuf::from).collect::<Vec<_>>();
            return if paths.is_empty() {
                StartupMode::Tray
            } else {
                StartupMode::OpenVideos(paths)
            };
        }

        let mut paths = vec![PathBuf::from(first)];
        paths.extend(args.map(PathBuf::from));
        if paths
            .iter()
            .all(|path| video_file_extension_supported(path))
        {
            StartupMode::OpenVideos(paths)
        } else {
            StartupMode::Tray
        }
    }

    fn video_file_extension_supported(path: &Path) -> bool {
        path.extension()
            .and_then(OsStr::to_str)
            .map(|extension| {
                let extension = format!(".{}", extension.to_ascii_lowercase());
                VIDEO_FILE_EXTENSIONS.contains(&extension.as_str())
            })
            .unwrap_or(false)
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

    fn refresh_default_game_list(paths: &GameListPaths) -> io::Result<()> {
        let contents = download_text(DEFAULT_GAME_LIST_URL)?;
        if !valid_default_game_list_download(&contents) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "downloaded default game list did not look valid",
            ));
        }

        write_file_atomically(&paths.default, contents.as_bytes())
    }

    fn download_text(url: &str) -> io::Result<String> {
        let bytes = download_bytes(url)?;
        String::from_utf8(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    fn download_bytes(url: &str) -> io::Result<Vec<u8>> {
        let agent = to_wide_null(APP_NAME);
        let session = unsafe {
            InternetOpenW(
                agent.as_ptr(),
                INTERNET_OPEN_TYPE_PRECONFIG,
                ptr::null(),
                ptr::null(),
                0,
            )
        };
        if session.is_null() {
            return Err(io::Error::last_os_error());
        }
        let session = InternetHandle(session);

        set_internet_timeout(session.0, INTERNET_OPTION_CONNECT_TIMEOUT);
        set_internet_timeout(session.0, INTERNET_OPTION_SEND_TIMEOUT);
        set_internet_timeout(session.0, INTERNET_OPTION_RECEIVE_TIMEOUT);

        let url = to_wide_null(url);
        let request = unsafe {
            InternetOpenUrlW(
                session.0,
                url.as_ptr(),
                ptr::null(),
                0,
                INTERNET_FLAG_RELOAD | INTERNET_FLAG_NO_CACHE_WRITE,
                0,
            )
        };
        if request.is_null() {
            return Err(io::Error::last_os_error());
        }
        let request = InternetHandle(request);

        let status = http_status_code(request.0)?;
        if status != HTTP_STATUS_OK {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("default game list download returned HTTP {status}"),
            ));
        }

        let mut bytes = Vec::new();
        let mut buffer = [0u8; DOWNLOAD_BUFFER_SIZE];
        loop {
            let mut read = 0;
            let ok = unsafe {
                InternetReadFile(
                    request.0,
                    buffer.as_mut_ptr() as *mut c_void,
                    buffer.len() as DWORD,
                    &mut read,
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            if read == 0 {
                break;
            }

            bytes.extend_from_slice(&buffer[..read as usize]);
        }

        Ok(bytes)
    }

    struct InternetHandle(*mut c_void);

    impl Drop for InternetHandle {
        fn drop(&mut self) {
            unsafe {
                InternetCloseHandle(self.0);
            }
        }
    }

    fn set_internet_timeout(handle: *mut c_void, option: DWORD) {
        let mut timeout = GAME_LIST_DOWNLOAD_TIMEOUT_MS;
        unsafe {
            InternetSetOptionW(
                handle,
                option,
                &mut timeout as *mut DWORD as *mut c_void,
                mem::size_of::<DWORD>() as DWORD,
            );
        }
    }

    fn http_status_code(request: *mut c_void) -> io::Result<DWORD> {
        let mut status = 0;
        let mut status_len = mem::size_of::<DWORD>() as DWORD;
        let mut index = 0;
        let ok = unsafe {
            HttpQueryInfoW(
                request,
                HTTP_QUERY_STATUS_CODE | HTTP_QUERY_FLAG_NUMBER,
                &mut status as *mut DWORD as *mut c_void,
                &mut status_len,
                &mut index,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(status)
    }

    fn valid_default_game_list_download(contents: &str) -> bool {
        let prefix = contents
            .trim_start()
            .chars()
            .take(512)
            .collect::<String>()
            .to_ascii_lowercase();
        if prefix.starts_with("404:") || prefix.starts_with("<!doctype") || prefix.contains("<html")
        {
            return false;
        }

        contents.lines().filter_map(normalize_game_name).count() >= MIN_DOWNLOADED_GAME_LIST_ENTRIES
    }

    fn write_file_atomically(path: &Path, contents: &[u8]) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let temp_path = path.with_extension("txt.download");
        fs::write(&temp_path, contents)?;
        if let Err(error) = fs::rename(&temp_path, path) {
            let _ = fs::remove_file(&temp_path);
            return Err(error);
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
        let mut hdr_snapshot = None;

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
                hdr_snapshot = enable_hdr_for_game().ok().flatten();
            } else if !is_running && was_running {
                if let Some(snapshot) = hdr_snapshot.take() {
                    let _ = restore_hdr_targets(&snapshot);
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

    fn open_videos_with_potplayer(paths: &[PathBuf]) -> io::Result<()> {
        let has_hdr_metadata = paths.iter().any(|path| video_file_has_hdr_metadata(path));
        let hdr_snapshot = if has_hdr_metadata {
            enable_hdr_for_game()?
        } else {
            let _ = disable_hdr_for_video();
            None
        };

        let launch_result = launch_potplayer_and_wait(paths);
        let restore_result = if let Some(snapshot) = hdr_snapshot {
            restore_hdr_targets(&snapshot)
        } else {
            Ok(())
        };

        launch_result.and(restore_result)
    }

    fn disable_hdr_for_video() -> io::Result<()> {
        let targets = active_hdr_targets()?;
        set_hdr_targets_enabled(&targets, false)
    }

    fn launch_potplayer_and_wait(paths: &[PathBuf]) -> io::Result<()> {
        let potplayer = find_potplayer_exe().unwrap_or_else(|| PathBuf::from("PotPlayer.exe"));
        let mut child = Command::new(potplayer).args(paths).spawn()?;
        let wait_result = child.wait();

        match wait_for_potplayer_to_close() {
            Ok(()) => wait_result.map(|_| ()),
            Err(error) => Err(error),
        }
    }

    fn wait_for_potplayer_to_close() -> io::Result<()> {
        loop {
            if matching_exact_processes(POTPLAYER_EXE_NAMES)?.is_empty() {
                return Ok(());
            }

            thread::sleep(POLL_INTERVAL);
        }
    }

    fn matching_exact_processes(process_names: &[&str]) -> io::Result<HashSet<String>> {
        let expected = process_names
            .iter()
            .map(|name| name.to_ascii_lowercase())
            .collect::<HashSet<_>>();
        let mut matches = HashSet::new();
        if expected.is_empty() {
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
            if expected.contains(&exe_name.to_ascii_lowercase()) {
                matches.insert(exe_name);
            }

            if unsafe { Process32NextW(snapshot, &mut entry) } == 0 {
                break;
            }
        }

        Ok(matches)
    }

    fn find_potplayer_exe() -> Option<PathBuf> {
        if let Some(path) = std::env::var_os(POTPLAYER_ENV_VAR).map(PathBuf::from) {
            if path.is_file() {
                return Some(path);
            }
        }

        for exe_name in POTPLAYER_EXE_NAMES {
            if let Some(path) = registry_app_path(exe_name) {
                return Some(path);
            }
        }

        for exe_name in POTPLAYER_EXE_NAMES {
            if let Some(path) = path_lookup(exe_name) {
                return Some(path);
            }
        }

        known_potplayer_locations()
            .into_iter()
            .find(|path| path.is_file())
    }

    fn registry_app_path(exe_name: &str) -> Option<PathBuf> {
        let subkeys = [
            format!(r"Software\Microsoft\Windows\CurrentVersion\App Paths\{exe_name}"),
            format!(r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\App Paths\{exe_name}"),
        ];

        for root in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
            for subkey in &subkeys {
                if let Ok(Some(path)) = registry_string(root, subkey, None) {
                    let path = PathBuf::from(path);
                    if path.is_file() {
                        return Some(path);
                    }
                }
            }
        }

        None
    }

    fn path_lookup(exe_name: &str) -> Option<PathBuf> {
        std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join(exe_name))
                .find(|candidate| candidate.is_file())
        })
    }

    fn known_potplayer_locations() -> Vec<PathBuf> {
        let mut locations = Vec::new();
        for env_var in ["ProgramFiles", "ProgramFiles(x86)"] {
            let Some(base) = std::env::var_os(env_var).map(PathBuf::from) else {
                continue;
            };

            for folder in [r"DAUM\PotPlayer", "PotPlayer"] {
                for exe_name in POTPLAYER_EXE_NAMES {
                    locations.push(base.join(folder).join(exe_name));
                }
            }
        }

        locations
    }

    fn video_file_has_hdr_metadata(path: &Path) -> bool {
        let extension = path
            .extension()
            .and_then(OsStr::to_str)
            .map(|extension| extension.to_ascii_lowercase())
            .unwrap_or_default();
        let include_tail_sample = matches!(extension.as_str(), "mp4" | "m4v" | "mov");
        let Ok(samples) =
            read_file_metadata_samples(path, MAX_VIDEO_METADATA_SCAN_BYTES, include_tail_sample)
        else {
            return false;
        };

        match extension.as_str() {
            "mp4" | "m4v" | "mov" => samples
                .iter()
                .any(|bytes| mp4_has_hdr_metadata(bytes) || hevc_annex_b_has_hdr_metadata(bytes)),
            "mkv" | "webm" => samples.iter().any(|bytes| {
                matroska_has_hdr_metadata(bytes) || hevc_annex_b_has_hdr_metadata(bytes)
            }),
            "m2ts" | "mts" | "ts" => samples.iter().any(|bytes| {
                mpeg_ts_has_hdr_metadata(bytes) || hevc_annex_b_has_hdr_metadata(bytes)
            }),
            _ => samples.iter().any(|bytes| {
                mp4_has_hdr_metadata(bytes)
                    || matroska_has_hdr_metadata(bytes)
                    || mpeg_ts_has_hdr_metadata(bytes)
                    || hevc_annex_b_has_hdr_metadata(bytes)
            }),
        }
    }

    fn read_file_metadata_samples(
        path: &Path,
        max_bytes: usize,
        include_tail_sample: bool,
    ) -> io::Result<Vec<Vec<u8>>> {
        let mut file = fs::File::open(path)?;
        let file_len = file.metadata()?.len();
        let mut prefix = Vec::new();
        file.by_ref()
            .take(max_bytes as u64)
            .read_to_end(&mut prefix)?;
        if !include_tail_sample || file_len <= max_bytes as u64 {
            return Ok(vec![prefix]);
        }

        let tail_start = file_len.saturating_sub(max_bytes as u64);
        file.seek(SeekFrom::Start(tail_start))?;
        let mut tail = Vec::new();
        file.take(max_bytes as u64).read_to_end(&mut tail)?;

        Ok(vec![prefix, tail])
    }

    fn mp4_has_hdr_metadata(bytes: &[u8]) -> bool {
        mp4_boxes_have_hdr_metadata(bytes, 0, bytes.len(), 0)
    }

    fn mp4_boxes_have_hdr_metadata(bytes: &[u8], start: usize, end: usize, depth: usize) -> bool {
        if depth > 12 || start >= end || end > bytes.len() {
            return false;
        }

        let mut position = start;
        while position + MP4_BOX_HEADER_SIZE <= end {
            let Some((box_type, payload_start, box_end)) = mp4_box_at(bytes, position, end) else {
                break;
            };

            if mp4_box_type_is_hdr_metadata(box_type) {
                return true;
            }
            if box_type == b"colr" && mp4_colr_box_is_hdr(&bytes[payload_start..box_end]) {
                return true;
            }
            if box_type == b"hvcC" && hevc_config_has_hdr_metadata(&bytes[payload_start..box_end]) {
                return true;
            }

            if let Some(child_start) = mp4_child_payload_start(box_type, payload_start, box_end) {
                if mp4_boxes_have_hdr_metadata(bytes, child_start, box_end, depth + 1) {
                    return true;
                }
            }

            if box_end <= position {
                break;
            }
            position = box_end;
        }

        false
    }

    fn mp4_box_at(bytes: &[u8], position: usize, limit: usize) -> Option<(&[u8], usize, usize)> {
        if position + MP4_BOX_HEADER_SIZE > limit {
            return None;
        }

        let size = read_be_u32(bytes, position)? as u64;
        let box_type = bytes.get(position + 4..position + 8)?;
        let (payload_start, box_end) = if size == 1 {
            if position + MP4_EXTENDED_BOX_HEADER_SIZE > limit {
                return None;
            }
            let size = read_be_u64(bytes, position + 8)?;
            if size < MP4_EXTENDED_BOX_HEADER_SIZE as u64 {
                return None;
            }
            (
                position + MP4_EXTENDED_BOX_HEADER_SIZE,
                position.checked_add(size as usize)?,
            )
        } else if size == 0 {
            (position + MP4_BOX_HEADER_SIZE, limit)
        } else {
            if size < MP4_BOX_HEADER_SIZE as u64 {
                return None;
            }
            (
                position + MP4_BOX_HEADER_SIZE,
                position.checked_add(size as usize)?,
            )
        };

        if payload_start > box_end || box_end > limit {
            return None;
        }

        Some((box_type, payload_start, box_end))
    }

    fn mp4_box_type_is_hdr_metadata(box_type: &[u8]) -> bool {
        matches!(box_type, b"mdcv" | b"clli" | b"dvcC" | b"dvvC")
    }

    fn mp4_colr_box_is_hdr(payload: &[u8]) -> bool {
        if payload.len() < 10 {
            return false;
        }
        if &payload[..4] != b"nclx" && &payload[..4] != b"nclc" {
            return false;
        }

        let primaries = read_be_u16(payload, 4).unwrap_or(0) as u64;
        let transfer = read_be_u16(payload, 6).unwrap_or(0) as u64;
        let matrix = read_be_u16(payload, 8).unwrap_or(0) as u64;
        hdr_colour_description(primaries, transfer, matrix)
    }

    fn mp4_child_payload_start(
        box_type: &[u8],
        payload_start: usize,
        box_end: usize,
    ) -> Option<usize> {
        let offset = match box_type {
            b"stsd" => payload_start.checked_add(8)?,
            b"meta" => payload_start.checked_add(4)?,
            b"av01" | b"avc1" | b"dvav" | b"dva1" | b"dvhe" | b"dvh1" | b"encv" | b"hev1"
            | b"hvc1" | b"mp4v" => payload_start.checked_add(78)?,
            b"edts" | b"mdia" | b"minf" | b"moof" | b"moov" | b"stbl" | b"traf" | b"trak"
            | b"udta" => payload_start,
            _ => return None,
        };

        (offset <= box_end).then_some(offset)
    }

    fn matroska_has_hdr_metadata(bytes: &[u8]) -> bool {
        matroska_unsigned_values(bytes, &[0x55, 0xba])
            .into_iter()
            .any(|transfer| matches!(transfer, HDR_TRANSFER_PQ | HDR_TRANSFER_HLG))
            || matroska_unsigned_values(bytes, &[0x55, 0xbb])
                .into_iter()
                .any(|primaries| primaries == HDR_COLOR_PRIMARIES_BT2020)
            || matroska_element_present(bytes, &[0x55, 0xd0])
            || matroska_element_present(bytes, &[0x55, 0xbc])
            || matroska_element_present(bytes, &[0x55, 0xbd])
    }

    fn matroska_unsigned_values(bytes: &[u8], id: &[u8]) -> Vec<u64> {
        let mut values = Vec::new();
        let mut position = 0;
        while let Some(index) = find_bytes(&bytes[position..], id) {
            let id_start = position + index;
            let size_start = id_start + id.len();
            if let Some((value_start, value_len)) = read_ebml_size(bytes, size_start) {
                if value_len <= 8 && value_start + value_len <= bytes.len() {
                    values.push(read_be_uint(&bytes[value_start..value_start + value_len]));
                }
            }
            position = id_start + id.len();
        }

        values
    }

    fn matroska_element_present(bytes: &[u8], id: &[u8]) -> bool {
        find_bytes(bytes, id).is_some()
    }

    fn read_ebml_size(bytes: &[u8], offset: usize) -> Option<(usize, usize)> {
        let first = *bytes.get(offset)?;
        let mut mask = 0x80;
        let mut length = 1;
        while length <= 8 && first & mask == 0 {
            mask >>= 1;
            length += 1;
        }
        if length > 8 || offset + length > bytes.len() {
            return None;
        }

        let mut value = (first & !mask) as usize;
        for byte in &bytes[offset + 1..offset + length] {
            value = (value << 8) | *byte as usize;
        }

        Some((offset + length, value))
    }

    fn mpeg_ts_has_hdr_metadata(bytes: &[u8]) -> bool {
        let Some((packet_size, sync_offset)) = detect_ts_packet_layout(bytes) else {
            return false;
        };

        let pmt_pids = find_ts_pmt_pids(bytes, packet_size, sync_offset);
        let video_pids = find_ts_hevc_pids(bytes, packet_size, sync_offset, &pmt_pids);
        if video_pids.is_empty() {
            return false;
        }

        let elementary_stream =
            extract_ts_elementary_stream(bytes, packet_size, sync_offset, &video_pids);
        hevc_annex_b_has_hdr_metadata(&elementary_stream)
    }

    fn detect_ts_packet_layout(bytes: &[u8]) -> Option<(usize, usize)> {
        for (packet_size, sync_offset) in [(TS_PACKET_SIZE, 0), (M2TS_PACKET_SIZE, 4)] {
            if bytes.len() < packet_size * 4 {
                continue;
            }

            let mut matched = true;
            for packet_index in 0..4 {
                if bytes.get(packet_index * packet_size + sync_offset) != Some(&0x47) {
                    matched = false;
                    break;
                }
            }

            if matched {
                return Some((packet_size, sync_offset));
            }
        }

        None
    }

    fn find_ts_pmt_pids(bytes: &[u8], packet_size: usize, sync_offset: usize) -> HashSet<u16> {
        let mut pids = HashSet::new();
        for packet in ts_packets(bytes, packet_size, sync_offset) {
            if packet.pid != 0 || !packet.payload_unit_start {
                continue;
            }
            if let Some(section) = psi_section(packet.payload) {
                parse_pat_section(section, &mut pids);
            }
        }

        pids
    }

    fn find_ts_hevc_pids(
        bytes: &[u8],
        packet_size: usize,
        sync_offset: usize,
        pmt_pids: &HashSet<u16>,
    ) -> HashSet<u16> {
        let mut video_pids = HashSet::new();
        for packet in ts_packets(bytes, packet_size, sync_offset) {
            if !pmt_pids.contains(&packet.pid) || !packet.payload_unit_start {
                continue;
            }
            if let Some(section) = psi_section(packet.payload) {
                parse_pmt_section(section, &mut video_pids);
            }
        }

        video_pids
    }

    struct TsPacket<'a> {
        pid: u16,
        payload_unit_start: bool,
        payload: &'a [u8],
    }

    fn ts_packets(bytes: &[u8], packet_size: usize, sync_offset: usize) -> Vec<TsPacket<'_>> {
        let mut packets = Vec::new();
        let mut position = 0;
        while position + packet_size <= bytes.len() {
            if let Some(packet) = ts_packet(bytes, position, packet_size, sync_offset) {
                packets.push(packet);
            }
            position += packet_size;
        }

        packets
    }

    fn ts_packet(
        bytes: &[u8],
        position: usize,
        packet_size: usize,
        sync_offset: usize,
    ) -> Option<TsPacket<'_>> {
        let header = position + sync_offset;
        if bytes.get(header) != Some(&0x47) || header + 4 > position + packet_size {
            return None;
        }

        let payload_unit_start = bytes[header + 1] & 0x40 != 0;
        let pid = (((bytes[header + 1] & 0x1f) as u16) << 8) | bytes[header + 2] as u16;
        let adaptation_field_control = (bytes[header + 3] >> 4) & 0x03;
        if adaptation_field_control == 0 || adaptation_field_control == 2 {
            return None;
        }

        let mut payload_start = header + 4;
        if adaptation_field_control == 3 {
            let adaptation_len = *bytes.get(payload_start)? as usize;
            payload_start = payload_start.checked_add(1 + adaptation_len)?;
        }

        let packet_end = position + packet_size;
        if payload_start > packet_end {
            return None;
        }

        Some(TsPacket {
            pid,
            payload_unit_start,
            payload: &bytes[payload_start..packet_end],
        })
    }

    fn psi_section(payload: &[u8]) -> Option<&[u8]> {
        let pointer = *payload.first()? as usize;
        let section_start = 1 + pointer;
        payload.get(section_start..)
    }

    fn parse_pat_section(section: &[u8], pids: &mut HashSet<u16>) {
        if section.len() < 12 || section[0] != 0x00 {
            return;
        }

        let section_len = (((section[1] & 0x0f) as usize) << 8) | section[2] as usize;
        let section_end = (3 + section_len).min(section.len());
        if section_end < 12 {
            return;
        }

        let mut position = 8;
        while position + 4 <= section_end.saturating_sub(4) {
            let program_number = read_be_u16(section, position).unwrap_or(0);
            let pid = (((section[position + 2] & 0x1f) as u16) << 8) | section[position + 3] as u16;
            if program_number != 0 {
                pids.insert(pid);
            }
            position += 4;
        }
    }

    fn parse_pmt_section(section: &[u8], video_pids: &mut HashSet<u16>) {
        if section.len() < 16 || section[0] != 0x02 {
            return;
        }

        let section_len = (((section[1] & 0x0f) as usize) << 8) | section[2] as usize;
        let section_end = (3 + section_len).min(section.len());
        if section_end < 16 {
            return;
        }

        let program_info_len = (((section[10] & 0x0f) as usize) << 8) | section[11] as usize;
        let mut position = 12 + program_info_len;
        while position + 5 <= section_end.saturating_sub(4) {
            let stream_type = section[position];
            let pid = (((section[position + 1] & 0x1f) as u16) << 8) | section[position + 2] as u16;
            let es_info_len =
                (((section[position + 3] & 0x0f) as usize) << 8) | section[position + 4] as usize;
            if stream_type == 0x24 {
                video_pids.insert(pid);
            }
            position += 5 + es_info_len;
        }
    }

    fn extract_ts_elementary_stream(
        bytes: &[u8],
        packet_size: usize,
        sync_offset: usize,
        video_pids: &HashSet<u16>,
    ) -> Vec<u8> {
        let mut stream = Vec::new();
        for packet in ts_packets(bytes, packet_size, sync_offset) {
            if !video_pids.contains(&packet.pid) {
                continue;
            }

            let payload = if packet.payload_unit_start {
                pes_payload(packet.payload).unwrap_or(packet.payload)
            } else {
                packet.payload
            };
            let remaining = MAX_TS_ELEMENTARY_STREAM_SCAN_BYTES.saturating_sub(stream.len());
            if remaining == 0 {
                break;
            }
            stream.extend_from_slice(&payload[..payload.len().min(remaining)]);
        }

        stream
    }

    fn pes_payload(payload: &[u8]) -> Option<&[u8]> {
        if payload.len() < 9 || payload[0..3] != [0x00, 0x00, 0x01] {
            return None;
        }

        let header_len = 9 + payload[8] as usize;
        payload.get(header_len..)
    }

    fn hevc_config_has_hdr_metadata(bytes: &[u8]) -> bool {
        if bytes.len() < 23 {
            return false;
        }

        let mut position = 23;
        let num_arrays = bytes[22] as usize;
        for _ in 0..num_arrays {
            if position + 3 > bytes.len() {
                return false;
            }
            let nal_type = bytes[position] & 0x3f;
            let nal_count = read_be_u16(bytes, position + 1).unwrap_or(0) as usize;
            position += 3;

            for _ in 0..nal_count {
                if position + 2 > bytes.len() {
                    return false;
                }
                let nal_len = read_be_u16(bytes, position).unwrap_or(0) as usize;
                position += 2;
                if position + nal_len > bytes.len() {
                    return false;
                }

                let nal = &bytes[position..position + nal_len];
                if hevc_nal_has_hdr_metadata(nal_type, nal) {
                    return true;
                }
                position += nal_len;
            }
        }

        false
    }

    fn hevc_annex_b_has_hdr_metadata(bytes: &[u8]) -> bool {
        let mut search_from = 0;
        while let Some((start_code, start_code_len)) = find_start_code(bytes, search_from) {
            let nal_start = start_code + start_code_len;
            let next = find_start_code(bytes, nal_start)
                .map(|(next_start, _)| next_start)
                .unwrap_or(bytes.len());
            if nal_start < next {
                let nal = &bytes[nal_start..next];
                if nal.len() >= 2 {
                    let nal_type = (nal[0] >> 1) & 0x3f;
                    if hevc_nal_has_hdr_metadata(nal_type, nal) {
                        return true;
                    }
                }
            }

            if next <= search_from {
                break;
            }
            search_from = next;
        }

        false
    }

    fn hevc_nal_has_hdr_metadata(nal_type: u8, nal: &[u8]) -> bool {
        match nal_type {
            HEVC_NAL_SPS => hevc_sps_has_hdr_metadata(nal),
            HEVC_NAL_PREFIX_SEI | HEVC_NAL_SUFFIX_SEI => hevc_sei_has_hdr_metadata(nal),
            _ => false,
        }
    }

    fn hevc_sei_has_hdr_metadata(nal: &[u8]) -> bool {
        if nal.len() <= 2 {
            return false;
        }

        let rbsp = ebsp_to_rbsp(&nal[2..]);
        let mut position = 0;
        while position + 1 < rbsp.len() {
            let (payload_type, type_len) = read_sei_value(&rbsp[position..]);
            position += type_len;
            if position >= rbsp.len() {
                break;
            }

            let (payload_size, size_len) = read_sei_value(&rbsp[position..]);
            position += size_len;
            let payload_size = payload_size as usize;
            if position + payload_size > rbsp.len() {
                break;
            }

            let payload = &rbsp[position..position + payload_size];
            if payload_type == HEVC_SEI_MASTERING_DISPLAY_COLOUR_VOLUME
                || payload_type == HEVC_SEI_CONTENT_LIGHT_LEVEL_INFO
                || (payload_type == HEVC_SEI_ALTERNATIVE_TRANSFER_CHARACTERISTICS
                    && payload.first() == Some(&(HDR_TRANSFER_HLG as u8)))
                || (payload_type == HEVC_SEI_USER_DATA_REGISTERED_ITU_T_T35
                    && itu_t_t35_payload_is_hdr_metadata(payload))
            {
                return true;
            }

            position += payload_size;
        }

        false
    }

    fn read_sei_value(bytes: &[u8]) -> (u64, usize) {
        let mut value = 0;
        let mut length = 0;
        while length < bytes.len() && bytes[length] == 0xff {
            value += 255;
            length += 1;
        }
        if length < bytes.len() {
            value += bytes[length] as u64;
            length += 1;
        }

        (value, length)
    }

    fn itu_t_t35_payload_is_hdr_metadata(payload: &[u8]) -> bool {
        matches!(
            payload,
            [0xb5, 0x00, 0x3c, 0x00 | 0x01 | 0x04, ..] | [0xb5, 0x00, 0x3b, ..]
        )
    }

    fn hevc_sps_has_hdr_metadata(nal: &[u8]) -> bool {
        if nal.len() <= 2 {
            return false;
        }

        let rbsp = ebsp_to_rbsp(&nal[2..]);
        let mut reader = BitReader::new(&rbsp);
        let Some(_) = reader.skip_bits(4) else {
            return false;
        };
        let Some(max_sub_layers_minus1) = reader.read_bits(3) else {
            return false;
        };
        let Some(_) = reader.skip_bits(1) else {
            return false;
        };
        if skip_profile_tier_level(&mut reader, max_sub_layers_minus1 as usize).is_none() {
            return false;
        }

        if reader.read_ue().is_none() {
            return false;
        }
        let Some(chroma_format_idc) = reader.read_ue() else {
            return false;
        };
        if chroma_format_idc == 3 && reader.skip_bits(1).is_none() {
            return false;
        }
        if reader.read_ue().is_none()
            || reader.read_ue().is_none()
            || skip_conformance_window(&mut reader).is_none()
            || reader.read_ue().is_none()
            || reader.read_ue().is_none()
        {
            return false;
        }
        let Some(log2_max_pic_order_cnt_lsb_minus4) = reader.read_ue() else {
            return false;
        };
        let Some(sub_layer_ordering_info_present) = reader.read_bit() else {
            return false;
        };
        let ordering_start = if sub_layer_ordering_info_present {
            0
        } else {
            max_sub_layers_minus1 as usize
        };
        for _ in ordering_start..=max_sub_layers_minus1 as usize {
            if reader.read_ue().is_none()
                || reader.read_ue().is_none()
                || reader.read_ue().is_none()
            {
                return false;
            }
        }
        for _ in 0..6 {
            if reader.read_ue().is_none() {
                return false;
            }
        }
        let Some(scaling_list_enabled) = reader.read_bit() else {
            return false;
        };
        if scaling_list_enabled {
            let Some(scaling_list_present) = reader.read_bit() else {
                return false;
            };
            if scaling_list_present && skip_scaling_list_data(&mut reader).is_none() {
                return false;
            }
        }
        if reader.skip_bits(2).is_none() {
            return false;
        }
        let Some(pcm_enabled) = reader.read_bit() else {
            return false;
        };
        if pcm_enabled
            && (reader.skip_bits(8).is_none()
                || reader.read_ue().is_none()
                || reader.read_ue().is_none()
                || reader.skip_bits(1).is_none())
        {
            return false;
        }

        let Some(num_short_term_ref_pic_sets) = reader.read_ue() else {
            return false;
        };
        let mut rps_delta_counts = Vec::new();
        for rps_index in 0..num_short_term_ref_pic_sets as usize {
            let Some(delta_count) =
                skip_short_term_ref_pic_set(&mut reader, rps_index, &rps_delta_counts)
            else {
                return false;
            };
            rps_delta_counts.push(delta_count);
        }

        let Some(long_term_ref_pics_present) = reader.read_bit() else {
            return false;
        };
        if long_term_ref_pics_present {
            let Some(num_long_term_ref_pics_sps) = reader.read_ue() else {
                return false;
            };
            for _ in 0..num_long_term_ref_pics_sps {
                if reader
                    .skip_bits((log2_max_pic_order_cnt_lsb_minus4 + 4) as usize + 1)
                    .is_none()
                {
                    return false;
                }
            }
        }

        if reader.skip_bits(2).is_none() {
            return false;
        }
        let Some(vui_parameters_present) = reader.read_bit() else {
            return false;
        };
        if !vui_parameters_present {
            return false;
        }

        hevc_vui_has_hdr_metadata(&mut reader)
    }

    fn skip_profile_tier_level(
        reader: &mut BitReader<'_>,
        max_sub_layers_minus1: usize,
    ) -> Option<()> {
        reader.skip_bits(96)?;
        let mut profile_present = [false; 8];
        let mut level_present = [false; 8];
        for index in 0..max_sub_layers_minus1 {
            profile_present[index] = reader.read_bit()?;
            level_present[index] = reader.read_bit()?;
        }
        if max_sub_layers_minus1 > 0 {
            for _ in max_sub_layers_minus1..8 {
                reader.skip_bits(2)?;
            }
        }
        for index in 0..max_sub_layers_minus1 {
            if profile_present[index] {
                reader.skip_bits(88)?;
            }
            if level_present[index] {
                reader.skip_bits(8)?;
            }
        }

        Some(())
    }

    fn skip_conformance_window(reader: &mut BitReader<'_>) -> Option<()> {
        if reader.read_bit()? {
            for _ in 0..4 {
                reader.read_ue()?;
            }
        }

        Some(())
    }

    fn skip_scaling_list_data(reader: &mut BitReader<'_>) -> Option<()> {
        for size_id in 0..4 {
            let matrix_count = if size_id == 3 { 2 } else { 6 };
            for _ in 0..matrix_count {
                if !reader.read_bit()? {
                    reader.read_ue()?;
                    continue;
                }

                let coef_count = 64.min(1usize << (4 + (size_id << 1)));
                if size_id > 1 {
                    reader.read_se()?;
                }
                for _ in 0..coef_count {
                    reader.read_se()?;
                }
            }
        }

        Some(())
    }

    fn skip_short_term_ref_pic_set(
        reader: &mut BitReader<'_>,
        rps_index: usize,
        previous_delta_counts: &[usize],
    ) -> Option<usize> {
        let inter_ref_pic_set_prediction = rps_index != 0 && reader.read_bit()?;
        if inter_ref_pic_set_prediction {
            reader.skip_bits(1)?;
            reader.read_ue()?;
            let previous_count = previous_delta_counts
                .get(rps_index.saturating_sub(1))
                .copied()
                .unwrap_or(0);
            let mut delta_count = 0;
            for _ in 0..=previous_count {
                let used_by_curr_pic = reader.read_bit()?;
                let use_delta_flag = if used_by_curr_pic {
                    false
                } else {
                    reader.read_bit()?
                };
                if used_by_curr_pic || use_delta_flag {
                    delta_count += 1;
                }
            }

            return Some(delta_count);
        }

        let num_negative_pics = reader.read_ue()? as usize;
        let num_positive_pics = reader.read_ue()? as usize;
        for _ in 0..num_negative_pics {
            reader.read_ue()?;
            reader.skip_bits(1)?;
        }
        for _ in 0..num_positive_pics {
            reader.read_ue()?;
            reader.skip_bits(1)?;
        }

        Some(num_negative_pics + num_positive_pics)
    }

    fn hevc_vui_has_hdr_metadata(reader: &mut BitReader<'_>) -> bool {
        let Some(aspect_ratio_info_present) = reader.read_bit() else {
            return false;
        };
        if aspect_ratio_info_present {
            let Some(aspect_ratio_idc) = reader.read_bits(8) else {
                return false;
            };
            if aspect_ratio_idc == 255 && reader.skip_bits(32).is_none() {
                return false;
            }
        }
        let Some(overscan_info_present) = reader.read_bit() else {
            return false;
        };
        if overscan_info_present && reader.skip_bits(1).is_none() {
            return false;
        }
        let Some(video_signal_type_present) = reader.read_bit() else {
            return false;
        };
        if !video_signal_type_present {
            return false;
        }

        if reader.skip_bits(4).is_none() {
            return false;
        }
        let Some(colour_description_present) = reader.read_bit() else {
            return false;
        };
        if !colour_description_present {
            return false;
        }

        let Some(primaries) = reader.read_bits(8) else {
            return false;
        };
        let Some(transfer) = reader.read_bits(8) else {
            return false;
        };
        let Some(matrix) = reader.read_bits(8) else {
            return false;
        };

        hdr_colour_description(primaries, transfer, matrix)
    }

    fn hdr_colour_description(primaries: u64, transfer: u64, matrix: u64) -> bool {
        matches!(transfer, HDR_TRANSFER_PQ | HDR_TRANSFER_HLG)
            || (primaries == HDR_COLOR_PRIMARIES_BT2020 && matrix == HDR_COLOR_PRIMARIES_BT2020)
    }

    struct BitReader<'a> {
        bytes: &'a [u8],
        bit_position: usize,
    }

    impl<'a> BitReader<'a> {
        fn new(bytes: &'a [u8]) -> Self {
            Self {
                bytes,
                bit_position: 0,
            }
        }

        fn read_bit(&mut self) -> Option<bool> {
            Some(self.read_bits(1)? != 0)
        }

        fn read_bits(&mut self, count: usize) -> Option<u64> {
            if count > 64 || self.bit_position + count > self.bytes.len() * 8 {
                return None;
            }

            let mut value = 0;
            for _ in 0..count {
                let byte = self.bytes[self.bit_position / 8];
                let shift = 7 - (self.bit_position % 8);
                value = (value << 1) | ((byte >> shift) & 1) as u64;
                self.bit_position += 1;
            }

            Some(value)
        }

        fn skip_bits(&mut self, count: usize) -> Option<()> {
            self.read_bits(count).map(|_| ())
        }

        fn read_ue(&mut self) -> Option<u64> {
            let mut leading_zero_bits = 0;
            while !self.read_bit()? {
                leading_zero_bits += 1;
                if leading_zero_bits > 63 {
                    return None;
                }
            }

            let suffix = if leading_zero_bits == 0 {
                0
            } else {
                self.read_bits(leading_zero_bits)?
            };
            Some((1u64 << leading_zero_bits) - 1 + suffix)
        }

        fn read_se(&mut self) -> Option<i64> {
            let code_num = self.read_ue()? as i64;
            if code_num % 2 == 0 {
                Some(-(code_num / 2))
            } else {
                Some((code_num + 1) / 2)
            }
        }
    }

    fn ebsp_to_rbsp(bytes: &[u8]) -> Vec<u8> {
        let mut rbsp = Vec::with_capacity(bytes.len());
        let mut zero_count = 0;
        for &byte in bytes {
            if zero_count >= 2 && byte == 0x03 {
                zero_count = 0;
                continue;
            }

            rbsp.push(byte);
            if byte == 0 {
                zero_count += 1;
            } else {
                zero_count = 0;
            }
        }

        rbsp
    }

    fn find_start_code(bytes: &[u8], from: usize) -> Option<(usize, usize)> {
        let mut index = from;
        while index + 3 <= bytes.len() {
            if bytes.get(index..index + 3) == Some(&[0x00, 0x00, 0x01]) {
                return Some((index, 3));
            }
            if index + 4 <= bytes.len()
                && bytes.get(index..index + 4) == Some(&[0x00, 0x00, 0x00, 0x01])
            {
                return Some((index, 4));
            }
            index += 1;
        }

        None
    }

    fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        if needle.is_empty() || needle.len() > haystack.len() {
            return None;
        }

        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    fn read_be_u16(bytes: &[u8], offset: usize) -> Option<u16> {
        Some(u16::from_be_bytes(
            bytes.get(offset..offset + 2)?.try_into().ok()?,
        ))
    }

    fn read_be_u32(bytes: &[u8], offset: usize) -> Option<u32> {
        Some(u32::from_be_bytes(
            bytes.get(offset..offset + 4)?.try_into().ok()?,
        ))
    }

    fn read_be_u64(bytes: &[u8], offset: usize) -> Option<u64> {
        Some(u64::from_be_bytes(
            bytes.get(offset..offset + 8)?.try_into().ok()?,
        ))
    }

    fn read_be_uint(bytes: &[u8]) -> u64 {
        bytes
            .iter()
            .fold(0, |value, byte| (value << 8) | *byte as u64)
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
                        let _ = toggle_windows_hdr();
                    }
                    _ => {}
                }
                0
            }
            WM_COMMAND => {
                match loword(wparam as usize) as usize {
                    MENU_TOGGLE_HDR => {
                        let _ = toggle_windows_hdr();
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
                    MENU_VIDEO_CONTEXT_MENU => {
                        let _ = set_video_context_menu_enabled(!video_context_menu_enabled());
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

        let toggle = to_wide_null(toggle_hdr_menu_text());
        let default_list = to_wide_null("Use default game list");
        let custom_list = to_wide_null("Use custom game list");
        let edit_custom_list = to_wide_null("Edit custom game list");
        let startup = to_wide_null("Run at Windows startup");
        let video_context_menu = to_wide_null("Explorer video context menu");
        let quit = to_wide_null("Quit");
        let current_game_list_flags = game_list_flags();
        let run_at_startup = startup_enabled();
        let video_context_menu_enabled = video_context_menu_enabled();
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
        AppendMenuW(
            menu,
            MF_STRING | checked_if(video_context_menu_enabled),
            MENU_VIDEO_CONTEXT_MENU,
            video_context_menu.as_ptr(),
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

    fn toggle_hdr_menu_text() -> &'static str {
        match windows_hdr_enabled() {
            Ok(true) => "Toggle HDR Off",
            Ok(false) => "Toggle HDR On",
            Err(_) => "Toggle HDR On",
        }
    }

    #[derive(Clone, Copy)]
    struct DisplayTarget {
        adapter_id: winapi::shared::ntdef::LUID,
        id: UINT,
    }

    #[repr(C)]
    struct DisplayConfigDeviceInfoHeaderRaw {
        packet_type: UINT,
        size: UINT,
        adapter_id: winapi::shared::ntdef::LUID,
        id: UINT,
    }

    #[repr(C)]
    struct DisplayConfigGetAdvancedColorInfo2 {
        header: DisplayConfigDeviceInfoHeaderRaw,
        value: UINT,
        _color_encoding: UINT,
        _bits_per_color_channel: UINT,
        _active_color_mode: UINT,
    }

    impl DisplayConfigGetAdvancedColorInfo2 {
        fn high_dynamic_range_supported(&self) -> bool {
            self.value & DISPLAYCONFIG_HDR_SUPPORTED_MASK != 0
        }

        fn high_dynamic_range_user_enabled(&self) -> bool {
            self.value & DISPLAYCONFIG_HDR_USER_ENABLED_MASK != 0
        }
    }

    #[repr(C)]
    struct DisplayConfigSetHdrState {
        header: DisplayConfigDeviceInfoHeaderRaw,
        value: UINT,
    }

    #[derive(Clone)]
    struct HdrTargetState {
        target: DisplayTarget,
        enabled: bool,
    }

    fn toggle_windows_hdr() -> io::Result<()> {
        let targets = active_hdr_targets()?;
        set_hdr_targets_enabled(&targets, !targets.iter().any(|target| target.enabled))
    }

    fn enable_hdr_for_game() -> io::Result<Option<Vec<HdrTargetState>>> {
        let snapshot = active_hdr_targets()?;
        let mut changed = false;
        let mut first_error = None;

        for target in &snapshot {
            if !target.enabled {
                match set_display_target_hdr_enabled(target.target, true) {
                    Ok(()) => changed = true,
                    Err(error) => {
                        if first_error.is_none() {
                            first_error = Some(error);
                        }
                    }
                }
            }
        }

        if changed {
            Ok(Some(snapshot))
        } else if let Some(error) = first_error {
            Err(error)
        } else {
            Ok(None)
        }
    }

    fn restore_hdr_targets(snapshot: &[HdrTargetState]) -> io::Result<()> {
        set_hdr_target_states(snapshot)
    }

    fn windows_hdr_enabled() -> io::Result<bool> {
        Ok(active_hdr_targets()?.iter().any(|target| target.enabled))
    }

    fn active_hdr_targets() -> io::Result<Vec<HdrTargetState>> {
        let mut targets = Vec::new();
        let mut seen = HashSet::new();

        for path in active_display_paths()? {
            let target = DisplayTarget {
                adapter_id: path.targetInfo.adapterId,
                id: path.targetInfo.id,
            };

            if !seen.insert(display_target_key(target)) {
                continue;
            }

            if let Some(enabled) = display_target_hdr_enabled(target)? {
                targets.push(HdrTargetState { target, enabled });
            }
        }

        Ok(targets)
    }

    fn active_display_paths() -> io::Result<Vec<DISPLAYCONFIG_PATH_INFO>> {
        for _ in 0..3 {
            let mut path_count = 0;
            let mut mode_count = 0;
            let status = unsafe {
                GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count)
            };

            if !windows_status_is(status, ERROR_SUCCESS) {
                return Err(io::Error::from_raw_os_error(status));
            }

            let mut paths: Vec<DISPLAYCONFIG_PATH_INFO> =
                vec![unsafe { mem::zeroed() }; path_count as usize];
            let mut modes: Vec<DISPLAYCONFIG_MODE_INFO> =
                vec![unsafe { mem::zeroed() }; mode_count as usize];
            let status = unsafe {
                QueryDisplayConfig(
                    QDC_ONLY_ACTIVE_PATHS,
                    &mut path_count,
                    paths.as_mut_ptr(),
                    &mut mode_count,
                    modes.as_mut_ptr(),
                    ptr::null_mut(),
                )
            };

            if windows_status_is(status, ERROR_SUCCESS) {
                paths.truncate(path_count as usize);
                return Ok(paths);
            }
            if !windows_status_is(status, ERROR_INSUFFICIENT_BUFFER) {
                return Err(io::Error::from_raw_os_error(status));
            }
        }

        Err(io::Error::from_raw_os_error(
            ERROR_INSUFFICIENT_BUFFER as i32,
        ))
    }

    fn display_target_hdr_enabled(target: DisplayTarget) -> io::Result<Option<bool>> {
        if let Some(enabled) = display_target_hdr_enabled_modern(target)? {
            return Ok(enabled);
        }

        display_target_hdr_enabled_legacy(target)
    }

    fn display_target_hdr_enabled_modern(
        target: DisplayTarget,
    ) -> io::Result<Option<Option<bool>>> {
        let mut info = DisplayConfigGetAdvancedColorInfo2 {
            header: raw_display_config_header(
                DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO_2_RAW,
                target,
                mem::size_of::<DisplayConfigGetAdvancedColorInfo2>(),
            ),
            value: 0,
            _color_encoding: 0,
            _bits_per_color_channel: 0,
            _active_color_mode: 0,
        };

        let status = unsafe {
            DisplayConfigGetDeviceInfo(
                &mut info.header as *mut _ as *mut DISPLAYCONFIG_DEVICE_INFO_HEADER,
            )
        };
        if windows_status_is(status, ERROR_SUCCESS) {
            if info.high_dynamic_range_supported() {
                Ok(Some(Some(info.high_dynamic_range_user_enabled())))
            } else {
                Ok(Some(None))
            }
        } else if displayconfig_hdr_api_unsupported(status) {
            Ok(None)
        } else {
            Err(io::Error::from_raw_os_error(status))
        }
    }

    fn display_target_hdr_enabled_legacy(target: DisplayTarget) -> io::Result<Option<bool>> {
        let mut info: DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO = unsafe { mem::zeroed() };
        info.header._type = DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO;
        info.header.size = mem::size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>() as UINT;
        info.header.adapterId = target.adapter_id;
        info.header.id = target.id;

        let status = unsafe { DisplayConfigGetDeviceInfo(&mut info.header) };
        if windows_status_is(status, ERROR_SUCCESS) {
            if info.advancedColorSupported() != 0 {
                Ok(Some(legacy_advanced_color_info_hdr_enabled(&info)))
            } else {
                Ok(None)
            }
        } else {
            Err(io::Error::from_raw_os_error(status))
        }
    }

    fn legacy_advanced_color_info_hdr_enabled(
        info: &DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO,
    ) -> bool {
        info.advancedColorEnabled() != 0
            && info.value & DISPLAYCONFIG_LEGACY_WIDE_COLOR_ENFORCED_MASK == 0
            && info.value & DISPLAYCONFIG_LEGACY_ADVANCED_COLOR_FORCE_DISABLED_MASK == 0
    }

    fn set_hdr_targets_enabled(targets: &[HdrTargetState], enabled: bool) -> io::Result<()> {
        let mut first_error = None;

        for target in targets {
            if target.enabled != enabled {
                match set_display_target_hdr_enabled(target.target, enabled) {
                    Ok(()) => {}
                    Err(error) => {
                        if first_error.is_none() {
                            first_error = Some(error);
                        }
                    }
                }
            }
        }

        if let Some(error) = first_error {
            Err(error)
        } else {
            Ok(())
        }
    }

    fn set_hdr_target_states(targets: &[HdrTargetState]) -> io::Result<()> {
        let mut first_error = None;

        for target in targets {
            match set_display_target_hdr_enabled(target.target, target.enabled) {
                Ok(()) => {}
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }

        if let Some(error) = first_error {
            Err(error)
        } else {
            Ok(())
        }
    }

    fn set_display_target_hdr_enabled(target: DisplayTarget, enabled: bool) -> io::Result<()> {
        if set_display_target_hdr_enabled_modern(target, enabled)? {
            return Ok(());
        }

        set_display_target_hdr_enabled_legacy(target, enabled)
    }

    fn set_display_target_hdr_enabled_modern(
        target: DisplayTarget,
        enabled: bool,
    ) -> io::Result<bool> {
        let mut state = DisplayConfigSetHdrState {
            header: raw_display_config_header(
                DISPLAYCONFIG_DEVICE_INFO_SET_HDR_STATE_RAW,
                target,
                mem::size_of::<DisplayConfigSetHdrState>(),
            ),
            value: if enabled {
                DISPLAYCONFIG_ENABLE_HDR_MASK
            } else {
                0
            },
        };

        let status = unsafe {
            DisplayConfigSetDeviceInfo(
                &mut state.header as *mut _ as *mut DISPLAYCONFIG_DEVICE_INFO_HEADER,
            )
        };
        if windows_status_is(status, ERROR_SUCCESS) {
            Ok(true)
        } else if displayconfig_hdr_api_unsupported(status) {
            Ok(false)
        } else {
            Err(io::Error::from_raw_os_error(status))
        }
    }

    fn set_display_target_hdr_enabled_legacy(
        target: DisplayTarget,
        enabled: bool,
    ) -> io::Result<()> {
        let mut state: DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE = unsafe { mem::zeroed() };
        state.header._type = DISPLAYCONFIG_DEVICE_INFO_SET_ADVANCED_COLOR_STATE;
        state.header.size = mem::size_of::<DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE>() as UINT;
        state.header.adapterId = target.adapter_id;
        state.header.id = target.id;
        state.set_enableAdvancedColor(if enabled { 1 } else { 0 });

        let status = unsafe { DisplayConfigSetDeviceInfo(&mut state.header) };
        if windows_status_is(status, ERROR_SUCCESS) {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(status))
        }
    }

    fn raw_display_config_header(
        packet_type: UINT,
        target: DisplayTarget,
        size: usize,
    ) -> DisplayConfigDeviceInfoHeaderRaw {
        DisplayConfigDeviceInfoHeaderRaw {
            packet_type,
            size: size as UINT,
            adapter_id: target.adapter_id,
            id: target.id,
        }
    }

    fn displayconfig_hdr_api_unsupported(status: i32) -> bool {
        windows_status_is(status, ERROR_INVALID_PARAMETER)
            || windows_status_is(status, ERROR_NOT_SUPPORTED)
    }

    fn display_target_key(target: DisplayTarget) -> (u32, i32, u32) {
        (
            target.adapter_id.LowPart,
            target.adapter_id.HighPart,
            target.id,
        )
    }

    fn game_list_flags() -> usize {
        ACTIVE_GAME_LIST_FLAGS
            .get()
            .map(|flags| flags.load(Ordering::SeqCst))
            .unwrap_or(INITIAL_GAME_LIST_FLAGS)
    }

    fn toggle_game_list_flag(flag: usize) {
        if let Some(active_flags) = ACTIVE_GAME_LIST_FLAGS.get() {
            let previous_flags = active_flags.fetch_xor(flag, Ordering::SeqCst);
            let new_flags = previous_flags ^ flag;
            let _ = save_game_list_flags(new_flags);
        }
    }

    fn load_saved_game_list_flags() -> Option<usize> {
        saved_game_list_flags().ok().map(sanitize_game_list_flags)
    }

    fn saved_game_list_flags() -> io::Result<usize> {
        let key = match open_settings_key(KEY_QUERY_VALUE) {
            Ok(key) => key,
            Err(error) if is_not_found(&error) => return Ok(INITIAL_GAME_LIST_FLAGS),
            Err(error) => return Err(error),
        };
        let name = to_wide_null(GAME_LIST_FLAGS_REGISTRY_VALUE_NAME);
        let mut value_type = 0;
        let mut flags: DWORD = 0;
        let mut byte_len = mem::size_of::<DWORD>() as DWORD;
        let status = unsafe {
            RegQueryValueExW(
                key.0,
                name.as_ptr(),
                ptr::null_mut(),
                &mut value_type,
                &mut flags as *mut DWORD as *mut u8,
                &mut byte_len,
            )
        };

        if registry_status_is(status, ERROR_FILE_NOT_FOUND) {
            return Ok(INITIAL_GAME_LIST_FLAGS);
        }
        if !registry_status_is(status, ERROR_SUCCESS) {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        if value_type != REG_DWORD || byte_len != mem::size_of::<DWORD>() as DWORD {
            return Ok(INITIAL_GAME_LIST_FLAGS);
        }

        Ok(flags as usize)
    }

    fn save_game_list_flags(flags: usize) -> io::Result<()> {
        let key = create_settings_key(KEY_SET_VALUE)?;
        let name = to_wide_null(GAME_LIST_FLAGS_REGISTRY_VALUE_NAME);
        let flags = sanitize_game_list_flags(flags) as DWORD;
        let byte_len = mem::size_of::<DWORD>() as DWORD;
        let status = unsafe {
            RegSetValueExW(
                key.0,
                name.as_ptr(),
                0,
                REG_DWORD,
                &flags as *const DWORD as *const u8,
                byte_len,
            )
        };

        if registry_status_is(status, ERROR_SUCCESS) {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(status as i32))
        }
    }

    fn sanitize_game_list_flags(flags: usize) -> usize {
        flags & ALL_GAME_LIST_FLAGS
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
            || registry_status_is(status, ERROR_PATH_NOT_FOUND)
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

    fn video_context_menu_enabled() -> bool {
        let Ok(current_exe) = std::env::current_exe() else {
            return false;
        };
        let Some(extension) = VIDEO_FILE_EXTENSIONS.first() else {
            return false;
        };

        let subkey = video_context_menu_command_subkey(extension);
        match registry_string(HKEY_CURRENT_USER, &subkey, None) {
            Ok(Some(command)) => command_exe_path(&command)
                .map(|path| same_path(&path, &current_exe))
                .unwrap_or(false),
            _ => false,
        }
    }

    fn set_video_context_menu_enabled(enabled: bool) -> io::Result<()> {
        if enabled {
            enable_video_context_menu()
        } else {
            disable_video_context_menu()
        }
    }

    fn enable_video_context_menu() -> io::Result<()> {
        let exe_path = std::env::current_exe()?;
        let command = video_open_command_wide(&exe_path);
        let app_command_subkey = format!(r"{APPLICATION_REGISTRY_SUBKEY}\shell\open\command");
        let app_command_key = create_current_user_key(&app_command_subkey, KEY_SET_VALUE)?;
        set_registry_string(&app_command_key, None, &command)?;

        for extension in VIDEO_FILE_EXTENSIONS {
            let supported_type_subkey = format!(r"{APPLICATION_REGISTRY_SUBKEY}\SupportedTypes");
            let supported_type_key =
                create_current_user_key(&supported_type_subkey, KEY_SET_VALUE)?;
            set_registry_string(&supported_type_key, Some(extension), &to_wide_null(""))?;

            let shell_subkey = video_context_menu_subkey(extension);
            let shell_key = create_current_user_key(&shell_subkey, KEY_SET_VALUE)?;
            set_registry_string(&shell_key, None, &to_wide_null(VIDEO_CONTEXT_MENU_TEXT))?;
            set_registry_string(&shell_key, Some("Icon"), &path_string_wide(&exe_path))?;

            let command_subkey = video_context_menu_command_subkey(extension);
            let command_key = create_current_user_key(&command_subkey, KEY_SET_VALUE)?;
            set_registry_string(&command_key, None, &command)?;
        }

        Ok(())
    }

    fn disable_video_context_menu() -> io::Result<()> {
        for extension in VIDEO_FILE_EXTENSIONS {
            delete_current_user_tree(&video_context_menu_subkey(extension))?;
        }
        delete_current_user_tree(APPLICATION_REGISTRY_SUBKEY)
    }

    fn video_context_menu_subkey(extension: &str) -> String {
        format!(
            r"Software\Classes\SystemFileAssociations\{extension}\shell\{VIDEO_CONTEXT_MENU_KEY_NAME}"
        )
    }

    fn video_context_menu_command_subkey(extension: &str) -> String {
        format!(r"{}\command", video_context_menu_subkey(extension))
    }

    struct RegistryKey(HKEY);

    impl Drop for RegistryKey {
        fn drop(&mut self) {
            unsafe {
                RegCloseKey(self.0);
            }
        }
    }

    fn open_registry_key(root: HKEY, subkey: &str, access: DWORD) -> io::Result<RegistryKey> {
        let subkey = to_wide_null(subkey);
        let mut key = ptr::null_mut();
        let status = unsafe { RegOpenKeyExW(root, subkey.as_ptr(), 0, access, &mut key) };

        if registry_status_is(status, ERROR_SUCCESS) {
            Ok(RegistryKey(key))
        } else {
            Err(io::Error::from_raw_os_error(status as i32))
        }
    }

    fn create_current_user_key(subkey: &str, access: DWORD) -> io::Result<RegistryKey> {
        let subkey = to_wide_null(subkey);
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

    fn registry_string(
        root: HKEY,
        subkey: &str,
        value_name: Option<&str>,
    ) -> io::Result<Option<String>> {
        let key = match open_registry_key(root, subkey, KEY_QUERY_VALUE) {
            Ok(key) => key,
            Err(error) if is_not_found(&error) => return Ok(None),
            Err(error) => return Err(error),
        };
        let value_name = value_name.map(to_wide_null);
        let value_name_ptr = value_name
            .as_ref()
            .map(|name| name.as_ptr())
            .unwrap_or(ptr::null());
        let mut value_type = 0;
        let mut byte_len = 0;
        let status = unsafe {
            RegQueryValueExW(
                key.0,
                value_name_ptr,
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
                value_name_ptr,
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

    fn set_registry_string(
        key: &RegistryKey,
        value_name: Option<&str>,
        value: &[u16],
    ) -> io::Result<()> {
        let value_name = value_name.map(to_wide_null);
        let value_name_ptr = value_name
            .as_ref()
            .map(|name| name.as_ptr())
            .unwrap_or(ptr::null());
        let byte_len = (value.len() * mem::size_of::<u16>()) as DWORD;
        let status = unsafe {
            RegSetValueExW(
                key.0,
                value_name_ptr,
                0,
                REG_SZ,
                value.as_ptr() as *const u8,
                byte_len,
            )
        };

        if registry_status_is(status, ERROR_SUCCESS) {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(status as i32))
        }
    }

    fn delete_current_user_tree(subkey: &str) -> io::Result<()> {
        let subkey = to_wide_null(subkey);
        let status = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, subkey.as_ptr()) };

        if registry_status_is(status, ERROR_SUCCESS)
            || registry_status_is(status, ERROR_FILE_NOT_FOUND)
        {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(status as i32))
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

    fn open_settings_key(access: DWORD) -> io::Result<RegistryKey> {
        let subkey = to_wide_null(SETTINGS_REGISTRY_SUBKEY);
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

    fn create_settings_key(access: DWORD) -> io::Result<RegistryKey> {
        let subkey = to_wide_null(SETTINGS_REGISTRY_SUBKEY);
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

    fn video_open_command_wide(exe_path: &Path) -> Vec<u16> {
        let mut command = Vec::new();
        command.push('"' as u16);
        command.extend(exe_path.as_os_str().encode_wide());
        command.push('"' as u16);
        command.push(' ' as u16);
        command.extend(OsStr::new(ARG_OPEN_VIDEO).encode_wide());
        command.push(' ' as u16);
        command.push('"' as u16);
        command.push('%' as u16);
        command.push('1' as u16);
        command.push('"' as u16);
        command.push(0);
        command
    }

    fn path_string_wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
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

    fn windows_status_is(status: i32, code: DWORD) -> bool {
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

        #[test]
        fn sanitize_game_list_flags_keeps_known_flags_only() {
            assert_eq!(sanitize_game_list_flags(usize::MAX), ALL_GAME_LIST_FLAGS);
            assert_eq!(sanitize_game_list_flags(0), 0);
        }

        #[test]
        fn valid_default_game_list_download_accepts_game_entries() {
            let mut contents = String::new();
            for index in 0..MIN_DOWNLOADED_GAME_LIST_ENTRIES {
                contents.push_str(&format!("game-{index}.exe\n"));
            }

            assert!(valid_default_game_list_download(&contents));
        }

        #[test]
        fn valid_default_game_list_download_rejects_error_pages() {
            assert!(!valid_default_game_list_download(
                "<!DOCTYPE html><html><body>Not a game list</body></html>"
            ));
            assert!(!valid_default_game_list_download("404: Not Found"));
        }

        #[test]
        fn write_file_atomically_replaces_existing_file() -> io::Result<()> {
            let dir = unique_temp_dir("atomic-write");
            fs::create_dir_all(&dir)?;
            let path = dir.join("games_default.txt");
            fs::write(&path, "old.exe\n")?;

            write_file_atomically(&path, b"new.exe\n")?;

            assert_eq!(fs::read_to_string(&path)?, "new.exe\n");
            assert!(!path.with_extension("txt.download").exists());
            fs::remove_dir_all(&dir)?;
            Ok(())
        }

        #[test]
        fn startup_mode_opens_supported_video_args() {
            let args = vec![OsString::from("movie.mkv")];
            match startup_mode(args) {
                StartupMode::OpenVideos(paths) => {
                    assert_eq!(paths, vec![PathBuf::from("movie.mkv")])
                }
                _ => panic!("expected video launch mode"),
            }
        }

        #[test]
        fn video_open_command_quotes_exe_and_file_argument() {
            let command = video_open_command_wide(Path::new(r"C:\Apps\hdr-auto.exe"));
            let command = String::from_utf16_lossy(&command[..command.len() - 1]);
            assert_eq!(command, r#""C:\Apps\hdr-auto.exe" --open-video "%1""#);
        }

        #[test]
        fn modern_advanced_color_info_separates_wcg_from_hdr() {
            let mut info: DisplayConfigGetAdvancedColorInfo2 = unsafe { mem::zeroed() };
            info.value = DISPLAYCONFIG_HDR_SUPPORTED_MASK | (1 << 6) | (1 << 7);

            assert!(info.high_dynamic_range_supported());
            assert!(!info.high_dynamic_range_user_enabled());

            info.value |= DISPLAYCONFIG_HDR_USER_ENABLED_MASK;
            assert!(info.high_dynamic_range_user_enabled());
        }

        #[test]
        fn legacy_advanced_color_info_ignores_wide_color_state() {
            let mut info: DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO = unsafe { mem::zeroed() };
            info.set_advancedColorSupported(1);
            info.set_advancedColorEnabled(1);
            assert!(legacy_advanced_color_info_hdr_enabled(&info));

            info.value |= DISPLAYCONFIG_LEGACY_WIDE_COLOR_ENFORCED_MASK;
            assert!(!legacy_advanced_color_info_hdr_enabled(&info));
        }

        #[test]
        fn mp4_colr_box_with_pq_transfer_is_hdr() {
            let mut colr = b"nclx".to_vec();
            colr.extend_from_slice(&9u16.to_be_bytes());
            colr.extend_from_slice(&16u16.to_be_bytes());
            colr.extend_from_slice(&9u16.to_be_bytes());
            colr.push(0);

            assert!(mp4_has_hdr_metadata(&mp4_box(*b"colr", colr)));
        }

        #[test]
        fn matroska_transfer_characteristics_detects_hdr() {
            let bytes = [0x55, 0xba, 0x81, 0x10];

            assert!(matroska_has_hdr_metadata(&bytes));
        }

        #[test]
        fn hevc_sei_mastering_display_detects_hdr() {
            let mut bytes = vec![0x00, 0x00, 0x01, 0x4e, 0x01];
            bytes.push(HEVC_SEI_MASTERING_DISPLAY_COLOUR_VOLUME as u8);
            bytes.push(1);
            bytes.push(0);

            assert!(hevc_annex_b_has_hdr_metadata(&bytes));
        }

        fn mp4_box(box_type: [u8; 4], payload: Vec<u8>) -> Vec<u8> {
            let size = (MP4_BOX_HEADER_SIZE + payload.len()) as u32;
            let mut bytes = Vec::new();
            bytes.extend_from_slice(&size.to_be_bytes());
            bytes.extend_from_slice(&box_type);
            bytes.extend_from_slice(&payload);
            bytes
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
