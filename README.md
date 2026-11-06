# IPv6 DDNS

面向 Windows 和 macOS 的 Rust IPv6 DDNS 客户端，通过 Cloudflare 同步 AAAA 记录。当前已实现 Windows 10/11 x64 托盘端，macOS 端计划开发。

## Windows 安装和使用

1. 打开 `dist/IPv6-DDNS.exe`，点击「安装服务」，在 Windows 授权窗口确认。安装后服务默认随系统启动，当前账户登录后会出现托盘图标。
2. 点击「展开设置」，填写完整域名（例如 `home.example.com`）和 Cloudflare API Token。给 Token 目标 Zone 的 **Zone → Zone → Read** 和 **Zone → DNS → Edit** 权限。
3. 默认使用手动配置或稳定自动配置的公网 IPv6。要指定地址后缀，勾选「按 IPv6 后缀筛选」，输入末尾的一段或多段十六进制，例如 `::8023` 或 `::abcd:1234`。匹配时忽略大小写和每段的前导零。两种方式都跳过临时地址。
4. 展开设置时，点击「保存设置」或点击「立即运行」保存并同步。收起设置时，「立即运行」使用已保存的配置。状态区显示同步结果、最近运行时间和下次检查时间。更改设置时，Token 留空会继续使用已保存的值。

取消勾选「开机自动更新」后，Windows 服务改为按需启动，定时同步停止，「立即运行」仍可使用。更改开机设置需要 Windows 管理员授权。关闭窗口会收进托盘，右键可以退出托盘。

后台每 5 分钟重新读取网卡地址并检查一次，失败后 1 分钟重试。多个地址符合条件时优先使用 Windows IPv6 接口跃点数较低的网卡，同一优先级下优先使用手动配置的地址。

若已安装旧版，先退出旧版托盘，用管理员 PowerShell 停止 `IPv6DDNS` 服务，再用新版可执行文件替换 `%ProgramFiles%\IPv6DDNS\IPv6-DDNS.exe` 并启动服务。旧版的域名和 Token 会继续使用，原来手选的地址会切换为默认稳定模式。

新建 AAAA 记录默认仅 DNS，TTL 为 Auto。同步已有 AAAA 时按需更新 IPv6 地址和 TTL，代理状态等其它设置保留。同名多条 AAAA 会提示先处理冲突。

- [Cloudflare Token 创建说明](https://developers.cloudflare.com/fundamentals/api/get-started/create-token/)
- [Cloudflare AAAA 记录接口](https://developers.cloudflare.com/api/resources/dns/subresources/records/)

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
