# iWAN 迁移

源项目：`ustc-iwan`，基线 `d62318c2161d3299ae6f23e7f40899260f10e79a`。源目录保持只读。
目标：clash-rs 内置 iWAN 出站；先通过 CLI 登录获取凭证，再在 YAML 中指定凭证目录和线路。

## 构建与当前产物

已编译的完整标准 release：`target/release/clash-rs`（macOS arm64，包含 TUN、WireGuard、iWAN 和内置面板）。
构建命令为 `cargo +1.95.0 build -p clash-rs --release --locked`，使用默认完整标准功能集。
源码和二进制中的 iWAN 出站目前仍限于 IPv4 TCP；启用 TUN 不改变这一协议支持范围。

最新 Linux 产物：`target/aarch64-unknown-linux-musl/release/clash-rs`，
在 OrbStack Alpine 3.23.6 arm64 原生编译，静态链接，完整默认功能集。
该版本包含本次真实联调发现的 Linux 网卡选择和可选认证 nonce 兼容修复；
macOS 产物已于 2026-10-14 同步重新构建，包含上述修复。
Alpine 运行目录为 `/home/x/clash-iwan-run/`，使用 root 运行其中的 `start.sh`。
脚本负责退出时清理本配置的 Linux 策略路由；具体实测记录见 `tasks.md`。

macOS ARM64 最新产物仍为 `target/release/clash-rs`，约 14 MiB。
动态依赖检查确认仅依赖 macOS 自带系统库和框架，不需要 Homebrew 运行库；
它不是 Linux musl 式的完全静态可执行文件。

## 使用

```sh
clash-rs iwan fetch --config-dir ~/.config/iwan
clash-rs iwan list --config-dir ~/.config/iwan
clash-rs -f config.yaml
```

`fetch` 输出登录链接。在浏览器完成统一认证，取消打开 iWAN.app，复制页面上
`com.panabit.mobile://oauth2redirect?...` 回调链接并粘贴到终端。
凭证保存在指定目录的 `servers.json`，兼容原客户端格式。Unix 文件权限为 0600；
写入先创建临时文件，再原子替换。不把密码写进 Clash YAML。`list` 不联网、不解密密码。
凭证到期后再次运行 `fetch`，然后重新加载 Clash 配置以替换现有会话。

```yaml
mixed-port: 7890
proxies:
  - name: USTC-iWAN
    type: iwan
    config-dir: ~/.config/iwan
    line: 教育网线路
    # bind: en0       # 可选：物理网卡名或本地 IPv4；默认检测物理网卡
    # mtu: 1380       # 576..2040，实际取服务端协商值与本值的较小者
    # encryption: 1  # 0=明文，1=XOR，默认 1
rules:
  - DOMAIN-SUFFIX,ustc.edu.cn,USTC-iWAN
  - MATCH,DIRECT
```

`line` 使用完整名称、唯一名称关键字，或引号包裹的从 1 开始的序号（如 `"2"`）。
关键字命中多个线路时报错，避免连接错误线路。相对目录按进程当前工作目录解析（不随 `-d` 自动变更）；
推荐使用绝对路径或 `~/` 路径。业务分流使用现有 `rules`、`rule-providers` 和代理组。

## 架构与映射

| 原实现 | 目标实现 | 决策 |
|---|---|---|
| core/auth、crypto、protocol、gcm | clash-iwan/src/core | 保留报文和密码算法 |
| core/netif | clash-iwan/src/core/netif.rs | 保留物理网卡检测与 UDP 绑定 |
| bin/oidc | clash-iwan/src/{oidc,controller,fetch,config}.rs | OIDC/PKCE、控制器签名、保存和读取凭证 |
| local_proxy + netstack | clash-iwan/src/session.rs + core/netstack | 共享隧道与 smoltcp TCP 栈；DuplexStream 直接接 Clash |
| CLI fetch/list | clash-bin 的 iwan 子命令 | 不启动 Clash 或创建默认 YAML |
| 线路选择 | clash-lib 原生 iwan Handler | 首次使用认证；会话结束后下次连接重建 |

没有外部客户端进程，也没有额外 SOCKS/HTTP 监听端口。每个出站实例维护一个工作线程、
一个隧道和多个 TCP socket。网络缓冲区有界，连接有超时；丢弃 Handler 后工作线程退出。
域名交给 Clash DNS 解析，目的地址为 IPv4。底层 UDP 默认绑定物理网卡以避免 TUN 环路。

本次范围与原用户态客户端一致：IPv4 TCP；不支持 UDP relay、IPv6、dialer-proxy。
原代码的 encryption=2 数据面仍按 XOR 处理，本次不暴露未经验证的 AES 模式。
不迁移源项目的 Linux TUN/系统路由管理，Clash 已有相应入口。

## 验证记录

见 [tasks.md](tasks.md)。单元测试不使用真实凭证；Alpine 联调使用用户授权的现有凭证，
已验证教育网线路、TUN、fake-IP、国内直连和 TW 出站。未重新执行浏览器 OIDC 登录。
