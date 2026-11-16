# Release notes draft: what the engine adds

For the window worker, who keeps the product's release notes. Plain words, as they would appear.

## 0.9.x · Your Isonomic ID

**New.** The first time you open Isonomic, it makes your Isonomic ID: how your apps, and later your other devices, know it's you. Its keys are made on this Mac and stay in your keychain.

**What you may see after an update (test builds only).** Test builds of Isonomic aren't signed by a registered developer yet. macOS ties keychain items to the exact build that made them, so after you update, macOS may ask once for each of your ID's keys:

> "isonomic-capsule" wants to sign using key "isonomic-id:capsule/…/person/device-signing" in your keychain.

and, for the recovery seed:

> "isonomic-capsule" wants to use your confidential information stored in "isonomic-id" in your keychain.

Enter your login password and choose **Always Allow**. **Deny** leaves your ID as it is, but Isonomic can't sign with it until you allow it. Signed releases won't ask.

When it happens: Isonomic only reads your ID's record to show it, which never asks. The question comes when Isonomic first signs with your ID after an update. In this build nothing signs yet; that starts when apps get their own grants.

**Known limit.** Your ID's recovery key is kept on this Mac until the recovery kit is ready, so it won't help yet if you lose the Mac.
