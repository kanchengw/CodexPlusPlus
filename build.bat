@echo off
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvarsall.bat" x64
cd /d "E:\projects\CodexPlusPlus"
cargo build --manifest-path apps/codex-plus-launcher/Cargo.toml --target x86_64-pc-windows-msvc
