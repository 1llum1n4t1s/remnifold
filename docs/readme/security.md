# Security

The API key grants access to one server installation. Use separate installations for separate trust boundaries. Keep tokens and keys private; redaction only reduces accidental capture, so review what you store.

## First strict installation

Use an existing trusted private directory where no other user can replace the download before execution. Windows bootstraps exist for 1.0.30 and later; choose 1.0.30–1.0.32. Replace the version and path, then run:

```powershell
$version = '<published-version>'
$bootstrap = 'C:\absolute\trusted\private\path\remnifold-bootstrap.exe'
Invoke-WebRequest -MaximumRedirection 0 -Uri "https://remnifold-updates.kagayoi.com/velopack/remnifold-bootstrap-$version-x86_64-pc-windows-msvc.exe" -OutFile $bootstrap
$signature = Get-AuthenticodeSignature -LiteralPath $bootstrap
if ($signature.Status -eq 'Valid' -and $signature.SignerCertificate.Subject -like 'CN=Open Source Developer Yuichiro Shinozaki*' -and $signature.TimeStamperCertificate) { & $bootstrap install --version $version --apply } else { Write-Error 'Signature verification failed. Do not run this file.' }
```

The signature must be valid, name the publisher and have an RFC 3161 timestamp. It proves the signer, not a product, version or accepted publication. `install` separately verifies publication with authenticated native GitHub CLI. Never substitute an unverified helper or bypass a failed check. For earlier versions, build from trusted source.

On macOS and Linux, review the trusted-source [Bash bootstrap](../../scripts/bootstrap-plugin.sh), save it outside the work directory, and run:

```sh
bash ./bootstrap-plugin.sh --version '<published-version>' --apply
```

It needs native GitHub CLI, curl, tar and `sha256sum` or `shasum`. Without `--apply` it verifies only; `--download-only` returns the verified archive without running it. The full trust limits are in the [security design](../design/security.md).
