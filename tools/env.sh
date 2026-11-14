# Build environment for Git Bash when the shell predates the rustup/zig installs.
export PATH="$HOME/.cargo/bin:$HOME/.rustup/toolchains/stable-x86_64-pc-windows-gnu/lib/rustlib/x86_64-pc-windows-gnu/bin/self-contained:$(dirname "$(ls "$LOCALAPPDATA"/Microsoft/WinGet/Packages/zig.zig_*/zig-*/zig.exe | head -1)"):$PATH"
