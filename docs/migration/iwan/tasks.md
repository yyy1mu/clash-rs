# 迁移进度

- [x] 梳理报文、凭证和用户态 TCP 数据面
- [x] 将协议兼容层迁入独立 clash-iwan crate
- [x] 接入 iwan fetch/list 命令和兼容的 servers.json
- [x] 接入原生出站配置、线路选择和共享会话
- [x] 补充线路选择、并发 TCP、大流量回压和半关闭测试
- [x] 工作区锁定依赖下 14 项测试及 Clippy `-D warnings` 通过
- [x] 修改文件的 rustfmt 检查与 git diff --check 通过
- [x] 主程序 check/build 通过（`--no-default-features --features ring --locked`）
- [x] 主程序 4 项测试通过（含 iwan 子命令及原有 CLI 兼容性）
- [x] 可执行文件冒烟测试通过：list、fetch --help、缺失凭证、无默认配置副作用、无凭证输出、普通/严格 YAML、非法 MTU/加密拒绝
- [x] 使用用户授权的现有凭证完成教育网线路真实认证和数据传输
- [ ] 浏览器 OIDC 登录端到端验证（本次复用现有凭证）

## 验证环境与命令

2026-10-13，macOS aarch64，Rust 1.95.0。系统 Homebrew rustc 缺少 LLVM 动态库，
验证使用已有 rustup 工具链；首次 Cargo 下载关闭 HTTP 多路复用后完成。

```sh
cargo +1.95.0 test -p clash-iwan --locked
cargo +1.95.0 clippy -p clash-iwan --all-targets -- -D warnings
cargo +1.95.0 check -p clash-rs --no-default-features --features ring --locked
cargo +1.95.0 build -p clash-rs --no-default-features --features ring --locked
cargo +1.95.0 test -p clash-rs --no-default-features --features ring --locked
```

当前 `target/debug/clash-rs` 是用于验证的 ring 最小功能构建，未启用 TUN 等可选功能。
完整功能 release 构建已完成，见下方记录；跨平台构建及真实 USTC 线路尚未验证。
所有 UDP 模拟测试仅使用回环地址；CLI 冒烟测试使用临时目录中的虚构凭证。

## 完整功能 release（2026-10-13）

- 命令：`cargo +1.95.0 build -p clash-rs --release --locked`，默认功能集，构建成功。
- 产物：`target/release/clash-rs`，macOS Apple Silicon / arm64，约 14 MiB，版本 0.10.10。
- 编译记录确认 clash-lib 启用：aws-lc-rs、dashboard、redir、shadowquic、shadowsocks、ssh、tailscale、tproxy、tuic、tun、wireguard、zero_copy；iWAN 为常规依赖，已内置。
- 内置面板构建成功。
- release 二进制 `--version`、`iwan --help`、`iwan list`、`iwan fetch --help`、缺失凭证报错验证通过。
- 启用 TUN 并包含 iWAN 节点的 YAML，在普通和严格模式的 `-t` 配置检查中通过；非法 MTU/加密配置被拒绝。
- 未启动 TUN 设备或更改系统路由；真实 USTC 登录、TUN 端到端流量仍需实际联调。
- SHA-256：`9df2b85aff51164184825d8166da814e36d7ec7a22a4cdafbfe71f235f9323dd`。

### WireGuard 架构对照

WireGuard 和 iWAN 均实现 Clash 的 `OutboundHandler`，由现有 TUN 入站、dispatcher 和规则系统转发到相应出站。
iWAN 保持独立的共享隧道会话及用户态 TCP 栈，无需复制 WireGuard 的密码学协议或另建系统 TUN 设备。
本次按完整功能集重新编译，未改动现有 TUN、DNS、入站或分流实现。

## Alpine musl 构建与真实联调（2026-10-13）

- 环境：OrbStack `alpine`，Alpine 3.23.6，aarch64，Rust 1.95.0 musl。
- 构建：`cargo +1.95.0 build -p clash-rs --release --locked -j 5`，默认完整功能集和内置面板。
- 二进制：静态链接 ELF aarch64，版本 0.10.10；Alpine `/home/x/clash-iwan-run/clash-rs`。
- Mac 副本：`target/aarch64-unknown-linux-musl/release/` 中的 `clash-rs`、`config.yaml`、`start.sh`。
- SHA-256：`7dfe9c27098624f9e6b4d31cc792bd26ef5f1cb7c1ccedc548b822688635f771`。
- 修复 Linux 网卡探测对未迁移 `core::route` 的引用，改为读取 `/proc/net/route`；测试覆盖有效默认路由、最低 metric 和无效行。
- 实际服务器 OPEN_ACK 不携带 nonce。恢复源客户端的可选 nonce 行为；存在时仍检查长度和值，仍校验响应类型和签名。增加兼容回归覆盖。
- Alpine 上 `cargo +1.95.0 test -p clash-iwan --release --locked`：15 项通过。
- 完整配置 `-t --strict-config` 通过；`iwan list` 正确读取四条现有线路。
- 实际使用教育网线路：SOCKS 访问 `www.ustc.edu.cn` 的真实 IP `218.22.21.21` 和域名均返回 HTTP 200，下载 39990 字节，日志命中 iWAN。
- Linux TUN `clashiwan0`，路由表 2468，mark 3389；无显式代理访问校内真实 IP 返回 HTTP 200，日志命中 iWAN。
- fake-IP：本地 DNS 返回 `198.18.0.2`；以该地址访问校内 HTTPS 返回 HTTP 200，日志回映射域名、解析真实 IP 并命中 iWAN。
- DNS 劫持：查询 `9.9.9.9:53` 返回 Clash 的 fake-IP。
- 国内直连：`https://223.5.5.5/dns-query` 返回预期的无查询参数 HTTP 400，日志命中 DIRECT/GeoIP。
- 国外分流：固定真实目标 `104.16.123.96` 访问 `www.cloudflare.com/cdn-cgi/trace` 返回 HTTP 200，日志命中 TW；地区结果 `loc=TW`、`colo=TPE`。
- 已通过交付 `start.sh` 重复验证 iWAN HTTP 200 和台湾出口，再发停止信号；进程退出，策略规则恢复测试前状态，TUN 设备消失。路由表比较仅有 IPv6 RA 自然倒计时变化。

### 环境适配及边界

- OrbStack/宿主机 DNS 会返回宿主机 fake-IP；验证必须结合真实 IP、明确的本地 fake-IP 和 Clash 日志，不能仅凭普通域名请求成功判定。
- `respect-rules: true` 下，TUN 启动时 DNS 出站再解析代理域名会发生循环。本次配置将 TW 连接地址设为已验证的 Cloudflare IP `104.16.231.132`，保留原 SNI 和 WebSocket Host。入口变化时需更新该地址。
- `proxy-server-nameserver` 改为加密 DNS，避免普通 DNS 被宿主机返回 fake-IP；主 DNS 保留 Cloudflare/Google DoH 并按规则走 TW。
- 当前上游程序直接退出后会遗留 Linux 策略规则。本次不扩展修改核心生命周期，交付 `start.sh` 清理该配置创建的规则；测试后没有残留策略规则。
- 凭证仅复制到 Alpine `/home/x/.config/iwan/servers.json`（0600），未加入 Git。含节点信息的运行配置保存在私有运行目录和 Git 忽略的产物目录。
- 仅实测教育网线路；其他三条线路只验证读取。未验证新的浏览器登录、iWAN UDP 或 IPv6；iWAN 数据面仍仅支持 IPv4 TCP。

## macOS 最新 release（2026-10-14）

- 使用 `cargo +1.95.0 build -p clash-rs --release --locked` 重新构建成功，包含真实联调后的可选 nonce 兼容修复和完整默认功能集（TUN、iWAN、内置面板等）。
- 产物：`target/release/clash-rs`，Mach-O ARM64，约 14 MiB，版本 0.10.10。
- `otool -L` 仅列出 macOS 系统框架 IOKit、CoreFoundation、SystemConfiguration、Security、CoreServices，以及系统 libiconv、libSystem；无 Homebrew 或其他第三方动态库依赖。不是完全静态链接。
- SHA-256：`efa7783e586cc188ae799027ea00262bae36f2313139ca8bc8ec1836e9f9bc76`。
- 二进制版本检查和 CLI 冒烟测试通过：线路列表、帮助、缺失凭证报错、无凭证输出、无默认配置副作用、TUN+iWAN 普通及严格配置解析、非法 MTU/加密配置拒绝。
- 本轮未在 macOS 启动 TUN 或更改系统路由；真实联网验证仍以上述 Alpine 记录为准。

## Clash Verge 配置合并排查与 macOS 代理实测（2026-10-14）

- 用户已将 Verge 的 `verge-mihomo-alpha` 替换为本次 clash-rs，SHA-256 与最新 macOS 产物一致；不能仅凭进程通用名称推断内核版本。
- Verge 日志记录实际失败为 `duplicate field device-id`：导入文件的 `tun.device-id` 与 Verge 注入的 `tun.device` 在 clash-rs 中是同一字段的别名。
- 交付 `target/release/config.verge-clash-rs.yaml`，统一使用 `device: utun1024`、列表形式 `dns-hijack`，保留 clash-rs 的 `route-all`。MMDB 改为本机产物目录的绝对路径，避免 Verge 工作目录变化。
- 已备份并修正用户导入的 iWAN profile；没有切换当前选中的稳定版内核、启动 TUN 或修改系统路由。
- 替换后的 alpha 二进制对修正版执行严格配置检查通过；合并 Verge 当前全局覆盖设置后的配置执行普通配置检查通过。Verge 注入的 Mihomo 专用字段由当前 clash-rs 忽略，不能据此认为对应 UI 功能已经适配。
- 使用该 alpha 二进制，在 macOS 独立 SOCKS 端口 17890、DNS 2053、API 19090、TUN 关闭条件下实际验证：校内域名 HTTP 200（39990 字节）、国外域名 HTTP 200、国内 DoH 无参数请求返回预期 HTTP 400；日志分别命中 iWAN/IPCIDR、TW/Match、DIRECT/GeoIP。
- 经测试代理访问 Cloudflare trace 确认 `loc=TW`、`colo=TPE`。测试进程已退出。
- 尚未验证 Verge 切换 alpha 后的 macOS TUN 接管；以上是配置合并检查与 macOS 显式代理真实联网结果。

## api.llm.ustc.edu.cn 专项复测（2026-10-14）

- Alpine 教育网 iWAN：SOCKS 域名访问首页 HTTP 200；真实地址 114.214.240.204 命中已有 114.214.192.0/18 校内规则。
- Alpine 本地 DNS 返回 fake-IP 198.18.0.2，明确使用该地址、不设置显式代理的 TUN HTTPS 请求连续三次 HTTP 200（23296 字节）；日志命中 iWAN。
- macOS 同一 release 独立运行，关闭 TUN，使用 SOCKS 17890 和显式 DOMAIN 规则：同一首页 HTTP 200（23296 字节），日志命中 iWAN[Domain]，绑定 en0。
- 两端未携带 API Key 的 /v1/models 请求均返回 HTTP 401；证明网络可达，不代表已验证用户 API 鉴权。
- 排查时 Verge 控制 API /version 返回 Mihomo v1.19.32，运行配置无 iWAN 出站；这是当前状态，不能用于断定此前 alpha 内核失败的原因。此前 Verge 日志出现配置校验占用和应用失败，尚需失败状态下的运行配置及 TUN 路径复现。
- 测试进程已退出，Alpine 策略规则恢复；没有切换当前 macOS Verge 内核或改变系统路由。
- 生成私有产物 config.iwan-tw.verified.yaml，加入 API 域名优先走 iWAN；另有带替换标记的 config.iwan-wg-vmess.template.yaml。仅用合成参数验证组合配置解析，未测试 WG/VMess 服务，按用户要求不继续这部分联调。

## Verge 控制连接与重载排查（2026-10-14）

- Verge 日志显示本地校验约 9–40 毫秒；之后调用 `/configs?force=true` 等待约 60 秒失败并重启服务。期间新的保存操作提示 validation already running。
- 旧版离线复现：YAML 的 `external-controller-unix` 被 CLI 默认的 None 无条件覆盖，控制 socket 未启动。CLI 显式指定 socket 时首次重载成功，但 YAML 不含地址会使后续 socket 消失。
- 含 DNS 的隔离实例还复现旧 DNS 未释放端口，新实例绑定失败；与配置解析无关。关闭 TUN 的实例在重载时仍尝试清理路由也是独立缺陷。
- 修复：仅在显式提供 CLI 控制地址时覆盖 YAML；重载缺省 IPC 地址时保留当前地址；DNS runner 保存并等待任务句柄，通过底层服务器 graceful shutdown 等待监听释放；禁用 TUN 时跳过路由清理。
- 离线回归脚本：`scripts/test-verge-reload.py <binary>`，分别测试 YAML/CLI 控制地址，每种连续重载五次并检查 IPC 和本地 fake-IP DNS。不会访问 WG/VMess 或启用 TUN。
- 修复后 macOS 完整 release 编译成功（3m58s），SHA-256：`bc2bda1c3fc8ce64977468ce032814ce7cd77aa9d5abccbca141d9bbaeda3e32`。
- 回归通过：YAML/CLI 两种模式共十次重载，逐次 IPC 和 fake-IP DNS 响应成功，整轮重载及探测约 21–25 毫秒。旧版同一脚本失败于 YAML IPC 未启动。
- 实际 iWAN 配置关闭 TUN、独立端口复测：重载 HTTP 204，约 15 毫秒；重载后控制 socket 可用，SOCKS 经 iWAN 访问 api.llm.ustc.edu.cn 首页 HTTP 200。
- 修正版配置严格校验和 git diff --check 通过。所有本轮测试进程已退出；未替换应用目录中的内核，未验证新内核在 Verge 内启用 macOS TUN 的端到端行为。

## Verge 实际服务副本诊断（2026-10-14 上午）

- 应用目录 alpha 与最新 release 校验值一致，但服务进程实际执行 `/Library/Application Support/clash-verge-service/cores/verge-mihomo-alpha`，控制 socket 为 `/var/run/clash-verge-service/users/501/verge-mihomo.sock`；不能以应用目录文件代替服务副本验证。
- 实际服务 API 返回 clash-rs 0.10.10，存在 iWAN 出站。订阅日志并通过 7897 访问目标，确认命中 iWAN[Domain]，失败原因为 `iwan: missing authentication nonce`。该错误字符串已不在最新源码/二进制中，表明运行行为仍为旧版；服务副本校验因 root 权限无法读取（sudo -n 要求密码），尚未替换。
- 同时 Verge TUN 和系统代理开关均关闭；macOS HTTP/HTTPS 代理指向 Shadowrocket MacPacketTunnel 的 1082 端口，iWAN 服务地址路由指向 utun8。属于另一个影响浏览器流量入口的因素，不能拿它替代上述直接认证失败证据。

## Alpine TUN 再次实测（2026-10-14）

- 用户明确要求 TUN 测试；使用 Alpine 既有 musl 产物（SHA-256 `7dfe9c27098624f9e6b4d31cc792bd26ef5f1cb7c1ccedc548b822688635f771`），不是新编译的 macOS 重载修复版本。
- 启动前无 clash 进程，仅三条系统基础策略规则。通过 start.sh 启动 clashiwan0，确认表 2468 默认路由、mark 3389 和 DNS 劫持规则生效。
- 所有 curl 均使用 --noproxy '*'。真实 IP 114.214.240.204 请求首页 HTTP 200、23296 字节；fake-IP 198.18.0.2 请求连续三次 HTTP 200、23296 字节，耗时约 0.587/0.074/0.063 秒。
- 不指定 --resolve 的普通域名请求亦 HTTP 200，remote_ip=198.18.0.2；日志显示回映射 api.llm.ustc.edu.cn → 114.214.240.204，命中 iWAN[IPCIDR]。
- 本地 127.0.0.1 DNS 和被劫持的 9.9.9.9 DNS 查询均返回 198.18.0.2；未携带 API Key 的 /v1/models HTTP 401。
- Cloudflare trace 通过 TUN 命中 TW[Match]，返回 colo=TPE、loc=TW。
- 测试结束后退出进程，clashiwan0 消失，策略规则与启动前 diff 相同。日志：Alpine /home/x/clash-iwan-run/tun-oct14.log。没有测试 WG/VMess。

## 阿里云 x86_64 部署与 TUN 实测（2026-10-14）

- 按用户要求通过 termark 连接 `aliyun101.200.220.128`（Debian 13、x86_64）。服务器已有 VPN、DNS 和业务服务，未改动这些服务。
- 本机 Rust 1.95.0 + cargo-zigbuild 构建完整默认功能集：`cargo +1.95.0 zigbuild -p clash-rs --release --locked --target x86_64-unknown-linux-musl -j 6`，7 分钟完成。
- 产物：静态链接 x86_64 ELF，版本 0.10.10，22631872 字节。SHA-256 `7a93a26bb128335f3c0c7a85fa31a62cc0fee3eab59a28bd82972012259be993`，上传后校验一致，严格配置检查通过。
- 远端部署目录 `/opt/clash-iwan-test/`（0700），复用服务器已有 `/root/.config/iwan/servers.json` 教育网线路；未上传本机凭证。
- 使用独立网络命名空间 cr-iwan-test、veth 10.254.198.0/30 与带注释的临时 NAT；TUN clashiwan0、表 2468、mark 3389 仅作用于该命名空间。主机转发原本已开启，无需修改 sysctl。180 秒超时保护和退出清理。
- 主机直接访问 114.214.240.204:443 超时；命名空间内明确禁用显式代理，经 TUN+iWAN 访问同一 IP 返回 HTTP 200。
- DNS 查询 127.0.0.1 与被劫持的 9.9.9.9 均返回 198.18.0.2；fake-IP 访问 api.llm.ustc.edu.cn 连续三次 HTTP 200、23296 字节；未携带 API Key 的 /v1/models 返回 HTTP 401。
- 日志确认真实 IP 命中 iWAN[IPCIDR]，域名命中 iWAN[Domain]。未测试 WG/VMess；其余规则为 DIRECT。
- 测试退出码 0；测试进程、命名空间、veth 和临时 NAT 均已清理，主机 IPv4 路由和策略规则前后 diff 一致。未设置开机启动或在主机启用全局 TUN。
- 远端保留 `clash-rs`、`config.yaml`、`test-tun.sh`、`dns-test.py`、`test.log`、`core.log`。本地结果 `target/aliyun-test/test.log`；可在远端运行 `cd /opt/clash-iwan-test && timeout --signal=TERM --kill-after=10s 180 ./test-tun.sh` 重复隔离测试。

### 阿里云关闭 DNS 劫持和 fake-IP 的 TUN 测试

- 配置 `target/aliyun-test/config.real-ip.yaml`，远端 `/opt/clash-iwan-test/config.real-ip.yaml`：tun.enable=true，dns-hijack=false，DNS redir-host，监听127.0.0.1:1053，无 fake-ip-range。
- 独立命名空间中保留系统 DNS（阿里云100.100.2.136/138），未修改系统 resolv.conf。核心DNS 1053和223.5.5.5:53查询均返回114.214.240.204；系统解析结果相同。日志确认外部UDP DNS走DIRECT，没有dport53劫持规则。
- curl --noproxy '*'，没有--resolve或显式代理，访问api.llm.ustc.edu.cn首页连续三次HTTP200、remote_ip=114.214.240.204、23296字节；未鉴权/v1/models返回401。HTTPS日志命中iWAN[Domain]。
- 测试退出0，命名空间、veth、NAT已清理，主机路由/策略规则前后相同。远端real-ip-test.log、real-ip-core.log；本地结果target/aliyun-test/real-ip-test.log。只验证本配置在该Linux隔离环境，不代表macOS Verge验证通过。
