# HDR-Auto
A system tray application that automatically toggles HDR on in the Windows settings when a game that supports HDR is run. It will toggle HDR off when the game exits. This is useful because leaving HDR on results in washed out colors in Windows (setting SDR content brightness in Windows settings does not fully fix this issue and TVs like LG OLEDs have a separate mode for HDR that adjusts the TV settings for HDR which causes further issues).

1. Download the HDR-Auto.exe from the releases page.
2. Run the program and look for the system tray icon to ensure it is running.

Whenever the application is run it will download the latest community curated list. You can also create a custom list using this format:
```text
007FirstLight.exe
DOOMTheDarkAges.exe
MonsterHunterWilds.exe
```

This list is case insensitive and the .exe is optional.

You may choose to use either the default list, the custom list, or both.

You may contribute to the community list by editing the games_default.txt and creating a pull request. Please ensure the game you add is not already in the list, and is listed as "Native support" on the [PCGamingWiki HDR Page](https://www.pcgamingwiki.com/wiki/List_of_games_that_support_high_dynamic_range_display_(HDR)). Also ensure the .exe name is correct by running it on your computer and checking the HDR toggles on as intended.
