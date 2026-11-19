## Download and install

- **macOS 11 or later (Apple Silicon or Intel)** — Download the `macos-universal.dmg`, open it, and drag **Disk Sweeper** onto **Applications**. If you prefer the ZIP, extract it and move **Disk Sweeper.app** to Applications.
- **Windows x64** — Download the `windows-x64.zip`, extract it, and run **Disk Sweeper.exe**. If Microsoft Defender SmartScreen warns about an unrecognized app, confirm you downloaded this release, then select **More info > Run anyway**.
- **Verify a download** — Compare its hash with the matching filename in `SHA256SUMS`. Use `shasum -a 256 <filename>` on macOS or `Get-FileHash -Algorithm SHA256 <filename>` in PowerShell.

The default macOS build is ad-hoc signed. If Gatekeeper blocks a trusted download, right-click **Disk Sweeper.app > Open** and confirm. On newer macOS versions, try opening it once, then choose **System Settings > Privacy & Security > Open Anyway**. As a Terminal alternative after verifying the download, remove quarantine from this app with `xattr -dr com.apple.quarantine "/Applications/Disk Sweeper.app"`.
