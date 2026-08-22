# Publishing to winget

diskutility ships as a **portable** winget package (a bare exe, command alias
`diskutility`). Package identifier: `viorizz.diskutility`. Submissions go to
[microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs).

## Scripts

| script | purpose |
|---|---|
| `make-manifests.ps1 -Version X.Y.Z` | writes `manifests/*.yaml` for a published release, pulling the SHA256 from that release's `checksums.txt` |
| `submit.ps1 -Version X.Y.Z -Token <PAT> [-First] [-DryRun]` | submits via `wingetcreate` (installs it if missing) |

`winget validate --manifest packaging/winget/manifests` checks the generated
files locally.

## First submission (one-time, manual)

```powershell
./packaging/winget/submit.ps1 -Version 0.4.6 -Token $env:WINGET_TOKEN -First
```

The token is a GitHub personal access token with the `public_repo` scope;
`wingetcreate` uses it to fork winget-pkgs and open the PR under your account.
New packages get a short manual review by Microsoft; after the PR merges,
`winget install viorizz.diskutility` works.

## Every later release (automatic)

`.github/workflows/release.yml` has a `winget` job that runs
`submit.ps1 -Version <tag>` after the GitHub release is published, **if** the
repository secret `WINGET_TOKEN` is set (Settings → Secrets → Actions). Without
the secret the job prints a notice and exits successfully, so releases never
fail because of winget.
