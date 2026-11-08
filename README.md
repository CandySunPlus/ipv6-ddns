# IPv6 DDNS

面向 Windows 和 macOS 的 Rust IPv6 DDNS 客户端，通过 Cloudflare 同步 AAAA 记录。Windows 版使用服务和托盘窗口，macOS 版使用开机启动的后台守护进程和菜单栏界面。

## Windows 安装和使用

1. 打开 `dist/IPv6-DDNS.exe`，点击「安装服务」，在 Windows 授权窗口确认。安装后服务默认随系统启动，当前账户登录后会出现托盘图标。
2. 点击「展开设置」，填写完整域名（例如 `home.example.com`）和 Cloudflare API Token。给 Token 目标 Zone 的 **Zone → Zone → Read** 和 **Zone → DNS → Edit** 权限。
3. 默认使用手动配置或稳定自动配置的公网 IPv6。要指定地址后缀，勾选「按 IPv6 后缀筛选」，输入末尾的一段或多段十六进制，例如 `::8023` 或 `::abcd:1234`。匹配时忽略大小写和每段的前导零。两种方式都跳过临时地址。
4. 展开设置时，点击「保存设置」或点击「立即运行」保存并同步。收起设置时，「立即运行」使用已保存的配置。状态区显示同步结果、最近运行时间和下次检查时间。更改设置时，Token 留空会继续使用已保存的值。

取消勾选「开机自动更新」后，Windows 服务改为按需启动，定时同步停止，「立即运行」仍可使用。更改开机设置需要 Windows 管理员授权。关闭窗口会收进托盘，右键可以退出托盘。

后台每 5 分钟重新读取网卡地址并检查一次，失败后 1 分钟重试。多个地址符合条件时优先使用 Windows IPv6 接口跃点数较低的网卡，同一优先级下优先使用手动配置的地址。

Windows 托盘诊断日志保存在 `%LOCALAPPDATA%\IPv6DDNS\Logs\tray.log`。记录程序启动和退出、窗口和托盘操作、休眠和唤醒通知、后台请求失败或耗时过长、界面响应超时及恢复；运行中每 5 分钟写入一次响应正常的心跳。界面卡住时，独立监测线程会记录当时的处理阶段。日志不记录域名、IPv6、Token、请求内容或异常载荷，单个文件约 1 MiB，保留两份轮转备份（`tray.log.1`、`tray.log.2`）。文件时间使用本机时间。日志用于定位问题，不会自动结束或重启进程。

若已安装旧版，先退出旧版托盘，用管理员 PowerShell 停止 `IPv6DDNS` 服务，再用新版可执行文件替换 `%ProgramFiles%\IPv6DDNS\IPv6-DDNS.exe` 并启动服务。旧版的域名和 Token 会继续使用，原来手选的地址会切换为默认稳定模式。

新建 AAAA 记录默认仅 DNS，TTL 为 Auto。同步已有 AAAA 时按需更新 IPv6 地址和 TTL，代理状态等其它设置保留。同名多条 AAAA 会提示先处理冲突。

- [Cloudflare Token 创建说明](https://developers.cloudflare.com/fundamentals/api/get-started/create-token/)
- [Cloudflare AAAA 记录接口](https://developers.cloudflare.com/api/resources/dns/subresources/records/)

## macOS 安装和使用

1. 双击 `dist/IPv6 DDNS.app`（或运行 `dist/IPv6-DDNS`）。它是菜单栏应用，不会出现在 Dock 中；点击菜单栏的网络图标选择「打开 IPv6 DDNS」，然后点击「安装服务」。在系统授权窗口确认后，应用会安装后台守护进程，并注册登录时启动的菜单栏程序。
2. 点击「展开设置」，填写完整域名（例如 `home.example.com`）和 Cloudflare API Token。给 Token 目标 Zone 的 **Zone → Zone → Read** 和 **Zone → DNS → Edit** 权限。
3. 默认使用手动配置或稳定自动配置（SLAAC）的公网 IPv6。要指定地址后缀，勾选「按 IPv6 后缀筛选」，输入末尾的一段或多段十六进制，例如 `::8023` 或 `::abcd:1234`。匹配时忽略大小写和每段的前导零。两种方式都跳过临时地址。
4. 展开设置时，点击「保存设置」或点击「立即运行」保存并同步。收起设置时，「立即运行」使用已保存的配置。窗口显示同步结果、最近运行时间和下次检查时间。更改设置时，Token 留空会继续使用已保存的值。

后台守护进程由 `com.ipv6ddns.daemon` LaunchDaemon 管理，以 root 身份随系统启动，**不要求用户登录**。菜单栏程序由 `com.ipv6ddns.tray` LaunchAgent 在用户登录后启动；关闭窗口只会隐藏窗口，可从菜单栏重新打开。两者通过 `/var/run/ipv6ddns.sock` 通信，只有安装时使用的用户账户可以访问。

取消勾选「开机自动更新」后，LaunchDaemon 不再随系统启动；打开菜单栏界面或点击「立即运行」时按需启动，定时同步停止。「立即运行」仍可使用。更改开机设置需要系统管理员授权。

后台每 5 分钟重新读取网卡地址并检查一次，失败后 1 分钟重试。多个地址符合条件时优先使用路由度量较低的网卡，同一优先级下优先使用手动配置的地址。AAAA 记录的创建、TTL 和同名冲突处理与 Windows 版相同。

安装位置：

- `/Library/PrivilegedHelperTools/IPv6-DDNS`
- `/Library/LaunchDaemons/com.ipv6ddns.daemon.plist`
- `/Library/Application Support/IPv6DDNS/`（配置和安装账户，仅 root 可读）
- `~/Library/LaunchAgents/com.ipv6ddns.tray.plist`

## 构建 macOS 版

需要 macOS、Xcode Command Line Tools 和 Rust 工具链。界面使用 AppKit；后台 HTTPS 使用 rustls。打包脚本使用 macOS 自带的 `codesign`（用于 ad-hoc 签名）和 `hdiutil`。

```sh
./scripts/package-macos.sh
open "dist/IPv6 DDNS 0.1.0.dmg"
```

脚本会执行 release 构建、生成 `dist/IPv6 DDNS.app` 并用 ad-hoc 签名，再打包出 `dist/IPv6 DDNS <版本>.dmg`。版本号取自 `assets/macos/Info.plist` 的 `CFBundleShortVersionString`。DMG 里是应用本身和指向 `/Applications` 的快捷方式，拖进去即可安装；只包含构建机架构（Apple Silicon 上为 arm64）。

应用使用 ad-hoc 签名且未公证，拷到其他 Mac 首次打开会被 Gatekeeper 拦截：右键点应用选「打开」，或在「系统设置 → 隐私与安全性」中允许。

只想要裸可执行文件时：

```sh
cargo test --locked
cargo build --locked --release
mkdir -p dist
cp target/release/ipv6-ddns dist/IPv6-DDNS
```

`.app` 是启动和安装入口。安装后，后台守护进程和登录时的菜单栏程序都使用 `/Library/PrivilegedHelperTools/IPv6-DDNS`，因此可以把 `.app` 移到「应用程序」或删除；菜单栏图标和配置不受影响。

## 卸载 macOS 版

打开窗口，点击「展开设置」→「卸载服务…」，确认后批准系统管理员授权。卸载会停止并移除 LaunchDaemon、LaunchAgent、保存的配置和安装文件。也可以运行 `dist/IPv6-DDNS --uninstall`（保留了裸二进制时）。卸载后当前菜单栏程序会退出。

## 在 WSL 中构建 Windows 版

需要 Rust、Windows GNU 目标，以及 WSL 内的 `x86_64-w64-mingw32-gcc` 和 `x86_64-w64-mingw32-windres`。下面的命令在项目目录运行。

```sh
rustup target add x86_64-pc-windows-gnu
cargo test --locked
cargo build --locked --release --target x86_64-pc-windows-gnu
mkdir -p dist
cp target/x86_64-pc-windows-gnu/release/ipv6-ddns.exe dist/IPv6-DDNS.exe
```

## 卸载 Windows 版

打开窗口，选择「展开设置」→「卸载服务…」，确认后批准 Windows 管理员授权。卸载会移除 Windows 服务、保存的配置和登录启动项，并在下次重启时删除安装文件。也可以退出托盘后运行 `dist/IPv6-DDNS.exe --uninstall`。
