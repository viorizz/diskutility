# Publishing to winget

diskutility ships as a **portable** winget package (a bare exe, no installer).
Submission targets [microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs).

## Easiest path: wingetcreate

After a GitHub release exists, run:

```powershell
winget install wingetcreate
wingetcreate new https://github.com/viorizz/diskutility/releases/download/v0.3.0/diskutility.exe
```

Answer the prompts with:
- PackageIdentifier: `viorizz.diskutility`
- InstallerType: `portable`
- Command alias: `diskutility`
- License: `MIT`

`wingetcreate` computes the SHA256, generates the three manifest files, and can
submit the PR to microsoft/winget-pkgs for you (needs a GitHub token; first-time
publishers go through a short manual review by Microsoft).

## Template

`manifests/` in this folder contains a filled-in template for v0.3.0 — only the
`InstallerSha256` placeholder needs the value from the release's `checksums.txt`.

For each new version: bump `PackageVersion`, update the URL + SHA256, resubmit
(`wingetcreate update viorizz.diskutility -u <new-url> -v <version> --submit`).
