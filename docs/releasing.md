# HEPA Release Packaging

HEPA v1.0.0 publishes prebuilt CLI binaries as GitHub Release assets so normal
users can install and run `hepa` without installing Rust first.

## Release Assets

The release workflow builds and uploads this primary matrix:

| Platform | Target | Asset |
| --- | --- | --- |
| macOS Apple Silicon | `aarch64-apple-darwin` | `hepa-v1.0.0-aarch64-apple-darwin.tar.gz` |
| macOS Intel | `x86_64-apple-darwin` | `hepa-v1.0.0-x86_64-apple-darwin.tar.gz` |
| Linux x64 | `x86_64-unknown-linux-gnu` | `hepa-v1.0.0-x86_64-unknown-linux-gnu.tar.gz` |
| Linux ARM64 | `aarch64-unknown-linux-gnu` | `hepa-v1.0.0-aarch64-unknown-linux-gnu.tar.gz` |
| Windows x64 | `x86_64-pc-windows-msvc` | `hepa-v1.0.0-x86_64-pc-windows-msvc.zip` |

Each archive contains the executable named `hepa` or `hepa.exe`, plus README and
license files. The workflow also uploads per-asset `.sha256` files and a
combined `SHA256SUMS.txt` manifest.

## Publishing

Publishing is handled by `.github/workflows/release.yml` on `v*` tags, including
the `v1.0.0` release tag. It can also be run manually with `workflow_dispatch`
for a selected tag. The workflow builds each target in GitHub Actions and then
uploads the packaged archives and checksum manifest to the matching GitHub
Release.

macOS Apple Silicon may be the first locally verified target when that is the
available release machine, but it is not the only release artifact. The GitHub
Release is expected to carry the complete primary matrix above.

## Installing A Prebuilt Binary

Download the archive for your platform from the GitHub Release, verify it
against `SHA256SUMS.txt`, extract it, and place `hepa` on your `PATH`.

Using HEPA's default Pi harness still requires Pi and a configured model route:
provider credentials for cloud models such as DeepSeek, or a tool-call-capable
loopback local endpoint such as llama.cpp with chat-template/tool-call support,
Ollama, or vLLM. Run `hepa doctor` before release stress runs; known-weak or
unverified generic local endpoints must be fixed or replaced before they count
as local-model release evidence. The v1.0.0 release gate is the Hermes-present
cloud Pi route; local-model-only heavy stress is tracked as post-release
hardening unless fresh local evidence is attached to a later release.

## Source Fallback

If a prebuilt binary is not available for your platform, clone the repository
and build from source:

```bash
cargo build --release -p hepa-cli
```

The source fallback requires the Rust toolchain. After building, copy
`target/release/hepa-cli` to a location on your `PATH` as `hepa`.
