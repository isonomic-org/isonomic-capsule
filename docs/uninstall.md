# Removing Isonomic from a Mac

Isonomic puts things in four places. Removing the first three removes the app; the fourth is your data.

| What | Where |
|---|---|
| The app | `/Applications/Isonomic.app`, or wherever you copied it |
| The background engine's login item | `~/Library/LaunchAgents/com.isonomic.engine.plist` |
| The engine's short socket folder, only on some setups | `/tmp/isonomic-<number>-<letters>/` (gone after a restart anyway) |
| **Your data**: chats, apps' data, downloaded models, logs | `~/Library/Application Support/Isonomic/` |

## To remove the app and keep your data

Keeping the data means reinstalling Isonomic later brings back your chats and models without downloading them again.

1. Quit Isonomic (Isonomic › Quit, or ⌘Q).
2. Open Terminal and stop the background engine, then remove its login item:

   ```sh
   launchctl bootout gui/$(id -u)/com.isonomic.engine
   rm ~/Library/LaunchAgents/com.isonomic.engine.plist
   ```

   The first command may say the engine was not running. That is fine.
3. Drag Isonomic from Applications to the Trash.

## To remove everything

Do the three steps above, then delete your data. **This cannot be undone**: it deletes every chat, every app's data and every downloaded model.

```sh
rm -rf ~/Library/Application\ Support/Isonomic
rm -rf /tmp/isonomic-$(id -u)-*
```

If you set `ISONOMIC_CAPSULE_DIR` to keep your data somewhere else, delete that folder instead.

## How much space it takes

Most of it is models: 0.5 GB to 1.3 GB for the small ones Isonomic starts with, up to several GB for larger ones. To see the total:

```sh
du -sh ~/Library/Application\ Support/Isonomic
```

What each folder inside holds is listed in [the Capsule's data across builds](capsule-format.md).
