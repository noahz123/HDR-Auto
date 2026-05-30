## What is HDR-Auto?
A system tray application that automatically toggles HDR on in the Windows settings when a game that supports HDR is run. It will also toggle HDR off when the game exits.

HDR-Auto can also be used from Windows Explorer for video files. When a video is opened with HDR-Auto, it checks the file for HDR metadata, turns Windows HDR on only when HDR metadata is detected, opens the file in PotPlayer, and restores HDR to its previous state when PotPlayer closes. Videos without HDR metadata are opened with Windows HDR off.

## Why use HDR-Auto?
Leaving HDR on results in washed out colors in Windows (setting SDR content brightness in Windows settings helps, but doesn't fully resolve the issue). Additionally, TVs such as LG OLEDs have a separate mode for HDR and it is recommended to leave the OLED Pixel Brightness setting at 100 for HDR mode. However, this brightness is too high for comfortable viewing during normal SDR computer use. With HDR-Auto you can set OLED Pixel Brightness to a comfortable level for normal computer use, while leaving it at 100 for HDR mode.

## How to use HDR-Auto
Download the hdr-auto.exe from the releases page. Run the program and look for the system tray icon to ensure it is running.

Whenever the application is run it will download the latest community curated list. You can also create a custom list using this format:
```text
007FirstLight.exe
DOOMTheDarkAges.exe
MonsterHunterWilds.exe
```

This list is case insensitive and the .exe is optional. You can edit your custom list by clicking "Edit custom game list" in the system tray menu. You may choose to use either the default list, the custom list, or both.

## Opening videos with HDR-Auto
Right-click the HDR-Auto tray icon and enable "Explorer video context menu". After that, supported video files will have an "Open with HDR Auto" right-click entry in Windows Explorer.

You can also register or unregister the Explorer entry from a terminal:
```powershell
hdr-auto.exe --install-video-context-menu
hdr-auto.exe --uninstall-video-context-menu
```

The Explorer command registers common video extensions including `.mp4`, `.mkv`, `.m2ts`, `.mts`, `.ts`, `.mov`, `.m4v`, `.avi`, `.wmv`, `.webm`, `.mpg`, and `.vob`.

HDR-Auto looks for PotPlayer in the `HDR_AUTO_POTPLAYER` environment variable, PotPlayer's Windows App Paths registry entries, `PATH`, and common Program Files install folders. If none of those are found, it falls back to launching `PotPlayer.exe`.

## Contributing
You may contribute to the community list by editing the games_default.txt and creating a pull request. Please ensure the game you add is not already in the list, and is listed as "Native support" on the [PCGamingWiki HDR Page](https://www.pcgamingwiki.com/wiki/List_of_games_that_support_high_dynamic_range_display_(HDR)). Also ensure the .exe name is correct by running it on your computer and checking the HDR toggles on as intended.
