# IPv6 DDNS

面向 Linux、Windows 和 macOS 的 Rust IPv6 DDNS 客户端，支持 **Cloudflare 和阿里云 DNS** 同步单条 AAAA 记录。Linux 使用真实终端 TUI 配置和独立 systemd 服务；Windows 使用服务和托盘窗口；macOS 使用 LaunchDaemon 和菜单栏界面。

## Linux 安装和使用

支持使用 systemd 的发行版，IPv6 检测依赖 iproute2（Debian / Ubuntu：`sudo apt install iproute2`）。下载对应架构的二进制，或在 Linux 上构建：

```sh
cargo build --locked --release
sudo target/release/ipv6-ddns install
sudo /usr/local/libexec/ipv6-ddns tui
```

发布目录中的 `ipv6-ddns-linux-aarch64` 与 `ipv6-ddns-linux-x86_64` 分别用于 ARM64 和 x86_64 Linux；直接用对应文件执行 `install` 即可。升级时再次执行新版二进制的 `install`，保留现有配置并重启服务。

`install` 安装并启动 `ipv6-ddns.service`，默认启用开机启动，**无需登录，也无需保持 TUI 打开**。TUI 的「服务」页也可显式安装、启动和停止服务；不会自动提权。

TUI 操作：

- `1`–`4`：切换状态、DNS 配置、IPv6、服务页面。
- `Tab` / `Shift+Tab` / `↑↓`：移动焦点；`←→`：切换服务商或地址策略。
- `Enter`：编辑字段或执行选中的操作；编辑时 `Esc` 取消、`Ctrl+U` 清空。
- `Space`：切换开机自动更新草稿；`Ctrl+S` 保存配置与自动启动设置，**不立即同步**。
- `F5`：只同步后台已保存的配置，不使用未保存草稿。
- `q` / `Ctrl+C`：退出 TUI，不停止后台服务。

界面使用终端默认背景；终端设置了透明背景时会保留透明效果。Token 和 Secret 隐藏显示，状态响应不返回凭据。修改设置时，仅同一已保存服务商的空凭据字段可以沿用旧值；更换阿里云 AccessKey ID 时必须同时填写新 Secret。

```sh
sudo /usr/local/libexec/ipv6-ddns status     # 不含凭据的 JSON 状态
sudo /usr/local/libexec/ipv6-ddns run        # 单次同步
sudo /usr/local/libexec/ipv6-ddns start
sudo /usr/local/libexec/ipv6-ddns stop
journalctl -u ipv6-ddns.service
sudo /usr/local/libexec/ipv6-ddns uninstall  # 停止并移除服务、配置和安装文件
```

安装位置：

- `/usr/local/libexec/ipv6-ddns`
- `/etc/systemd/system/ipv6-ddns.service`
- `/etc/ipv6-ddns/config.json`（root 所有，目录 `0700`、文件 `0600`）
- `/run/ipv6-ddns/control.sock`（仅 root 可访问）

后台每 5 分钟检查，失败后 1 分钟重试。关闭「开机自动更新」并保存后，不再随系统启动，周期同步停止，但当前后台进程仍可接收手动同步请求。地址选择排除临时、tentative、重复检测失败、deprecated 和首选寿命为零的地址；优先选择 IPv6 默认路由 metric 较小的接口。

## DNS 服务商与权限

三端都在设置中选择服务商并填写完整域名，支持根域名和多级子域名。旧版 Cloudflare 配置无需重新填写 Token；读取时自动识别，下一次保存使用新配置格式。切换服务商不会复用另一家的凭据。

### 阿里云 DNS

填写 RAM 用户的 **AccessKey ID** 和 **AccessKey Secret**，不要使用主账号密钥。所需操作：

- `alidns:DescribeDomains`：查询账户实际托管的精确域名，需要 `Resource: *`。
- `alidns:DescribeSubDomainRecords`：读取目标 AAAA。
- `alidns:AddDomainRecord`、`alidns:UpdateDomainRecord`：新增或修改目标记录；按官方支持的资源粒度限制到目标域名。

不需要删除记录权限。自动查找最长匹配的托管域名，不按最后两段猜测根域名，支持独立托管的子域名。只管理**默认解析线路**的单条 AAAA：缺失时创建，TTL 为 600 秒；已有记录只按需修改地址，保留 TTL。重复、停用、锁定或不完整的记录列表会报错，不会自动删除或启用记录。

HTTPS 请求使用 RPC HMAC-SHA1 签名，凭据和签名放在 POST 表单中，不放入 URL；错误不输出密钥或服务端原始报错内容。

- [DescribeDomains / RAM 权限](https://www.alibabacloud.com/help/en/dns/api-alidns-2015-01-09-describedomains)
- [DescribeSubDomainRecords](https://www.alibabacloud.com/help/en/dns/api-alidns-2015-01-09-describesubdomainrecords)
- [AddDomainRecord](https://www.alibabacloud.com/help/en/dns/api-alidns-2015-01-09-adddomainrecord)
- [UpdateDomainRecord](https://www.alibabacloud.com/help/en/dns/api-alidns-2015-01-09-updatedomainrecord)

### Cloudflare

填写 API Token，授予目标 Zone 的 **Zone → Zone → Read** 和 **Zone → DNS → Edit** 权限。新建记录为仅 DNS、TTL Auto；已有记录保留代理等其它属性，按需更新地址和 TTL。同名多条 AAAA 会报错。

## Windows 安装和使用

1. 打开 `dist/IPv6-DDNS.exe`，点击「安装服务」，在 Windows 授权窗口确认。安装后服务默认随系统启动，当前账户登录后会出现托盘图标。
2. 点击「展开设置」，选择 Cloudflare 或阿里云 DNS，填写完整域名（例如 `home.example.com`）及对应凭据；权限见上文。
3. 默认使用手动配置或稳定自动配置的公网 IPv6。要指定地址后缀，勾选「按 IPv6 后缀筛选」，输入末尾的一段或多段十六进制，例如 `::8023` 或 `::abcd:1234`。匹配时忽略大小写和每段的前导零。两种方式都跳过临时地址。
4. 点击「保存设置」保存配置；「立即运行」始终只使用已保存配置，不会保存未提交的编辑。状态区显示同步结果、最近运行和下次检查时间。Token / Secret 留空只会沿用同一服务商已保存的值。

取消勾选「开机自动更新」后，Windows 服务改为按需启动，定时同步停止，「立即运行」仍可使用。更改开机设置需要 Windows 管理员授权。关闭窗口会收进托盘，右键可以退出托盘。

后台每 5 分钟重新读取网卡地址并检查一次，失败后 1 分钟重试。多个地址符合条件时优先使用 Windows IPv6 接口跃点数较低的网卡，同一优先级下优先使用手动配置的地址。

Windows 托盘诊断日志保存在 `%LOCALAPPDATA%\IPv6DDNS\Logs\tray.log`。记录程序启动和退出、窗口和托盘操作、休眠和唤醒通知、后台请求失败或耗时过长、界面响应超时及恢复；运行中每 5 分钟写入一次响应正常的心跳。界面卡住时，独立监测线程会记录当时的处理阶段。日志不记录域名、IPv6、Token、请求内容或异常载荷，单个文件约 1 MiB，保留两份轮转备份（`tray.log.1`、`tray.log.2`）。文件时间使用本机时间。日志用于定位问题，不会自动结束或重启进程。

若已安装旧版，先退出旧版托盘，用管理员 PowerShell 停止 `IPv6DDNS` 服务，再用新版可执行文件替换 `%ProgramFiles%\IPv6DDNS\IPv6-DDNS.exe` 并启动服务。旧版的域名和 Token 会继续使用，原来手选的地址会切换为默认稳定模式。

Cloudflare 与阿里云的记录创建、TTL 和冲突处理见「DNS 服务商与权限」。

- [Cloudflare Token 创建说明](https://developers.cloudflare.com/fundamentals/api/get-started/create-token/)
- [Cloudflare AAAA 记录接口](https://developers.cloudflare.com/api/resources/dns/subresources/records/)

## macOS 安装和使用

1. 双击 `dist/IPv6 DDNS.app`（或运行 `dist/IPv6-DDNS`）。它是菜单栏应用，不会出现在 Dock 中；点击菜单栏的网络图标选择「打开 IPv6 DDNS」，然后点击「安装服务」。在系统授权窗口确认后，应用会安装后台守护进程，并注册登录时启动的菜单栏程序。
2. 点击「展开设置」，选择 Cloudflare 或阿里云 DNS，填写完整域名（例如 `home.example.com`）及对应凭据；权限见上文。
3. 默认使用手动配置或稳定自动配置（SLAAC）的公网 IPv6。要指定地址后缀，勾选「按 IPv6 后缀筛选」，输入末尾的一段或多段十六进制，例如 `::8023` 或 `::abcd:1234`。匹配时忽略大小写和每段的前导零。两种方式都跳过临时地址。
4. 点击「保存设置」保存配置；「立即运行」始终只使用已保存配置，不会保存未提交的编辑。窗口显示同步结果、最近运行和下次检查时间。Token / Secret 留空只会沿用同一服务商已保存的值。

后台守护进程由 `com.ipv6ddns.daemon` LaunchDaemon 管理，以 root 身份随系统启动，**不要求用户登录**。菜单栏程序由 `com.ipv6ddns.tray` LaunchAgent 在用户登录后启动；关闭窗口只会隐藏窗口，可从菜单栏重新打开。两者通过 `/var/run/ipv6ddns.sock` 通信，只有安装时使用的用户账户可以访问。

取消勾选「开机自动更新」后，LaunchDaemon 不再随系统启动；打开菜单栏界面或点击「立即运行」时按需启动，定时同步停止。「立即运行」仍可使用。更改开机设置需要系统管理员授权。

后台每 5 分钟重新读取网卡地址并检查一次，失败后 1 分钟重试。多个地址符合条件时优先使用路由度量较低的网卡，同一优先级下优先使用手动配置的地址。两种 DNS 服务商的记录处理与 Windows / Linux 版相同。

安装位置：

- `/Library/PrivilegedHelperTools/IPv6-DDNS`
- `/Library/LaunchDaemons/com.ipv6ddns.daemon.plist`
- `/Library/Application Support/IPv6DDNS/`（配置和安装账户，仅 root 可读）
- `~/Library/LaunchAgents/com.ipv6ddns.tray.plist`

### 从已安装的 macOS 旧版升级

仅替换 `.app` 不会更新已安装的守护进程。先在菜单栏退出旧版托盘（不要卸载），然后在项目目录执行：

```sh
sudo "dist/IPv6 DDNS.app/Contents/MacOS/IPv6-DDNS" --install-admin "$(id -u)"
```

再打开新版 `.app`。安装器会清理旧控制通道并重新注册后台服务，不会删除域名或凭据；新界面与后台程序需要一起升级。安装器默认启用开机自动更新，如不需要可在升级后的设置中关闭。


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

## 界面决策

已确认采用紧凑原生桌面窗口和键盘操作的透明背景 Linux TUI。桌面版的 IPv6 后缀标签与输入框同行垂直居中。交互原型保存在独立分支 `prototype/approved-platform-ui`，不参与正式构建。
