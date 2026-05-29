# HDR Auto

A small Windows tray app that watches for configured game processes and sends `Win+Alt+B` when a configured game starts, then sends it again after the last configured game exits.

## Configure

HDR Auto can read `games_default.txt`, `games_custom.txt`, both, or neither. Put one game process executable per line:

```text
eldenring.exe
Cyberpunk2077.exe
bg3.exe
```

The `.exe` extension is optional and matching is case-insensitive.

Use `games_default.txt` for the bundled list and `games_custom.txt` for your own list. The app starts with the default list enabled; right-click the tray icon to enable or disable either list independently. If the same game appears in both enabled lists, HDR Auto treats it as one configured game.

HDR Auto only toggles back if it toggled HDR on. If you start HDR Auto while a listed game is already running, it treats that as the baseline and will not toggle HDR when that game exits.

Right-click the tray icon and check `Run at Windows startup` to start HDR Auto automatically when you sign in. Uncheck it to remove the startup entry.

## Run

```powershell
cargo run --release
```

The app appears in the system tray. Right-click the tray icon for `Toggle HDR now`, `Use default game list`, `Use custom game list`, `Run at Windows startup`, or `Quit`.
