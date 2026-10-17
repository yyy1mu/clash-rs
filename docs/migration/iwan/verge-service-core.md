# Clash Verge 服务内核核验与安全升级

`tasks.md` 的 2026-10-14 记录只证明隔离实例通过 YAML/CLI 共十次重载，未证明服务目录副本已更新，也未证明 macOS Verge TUN 可用。相同的 `0.10.10` 版本号不能区分这些构建。请以当前机器的进程路径、磁盘 SHA-256 和重启后的行为共同核验。

工具：`scripts/verge-service-core.py`，Python **3.11+**，仅依赖标准库。默认只读，不调用 sudo，不停止/重启服务，不操作控制 API、系统代理、TUN 或路由。写入操作仅支持 macOS，必须显式以管理员身份调用；Linux 可以执行文件核验和临时目录测试。

## 1. 默认只读核验

先自行构建或解压可信的 macOS release 可执行文件，例如 `target/release/clash-rs`。不要把压缩包作为输入，也不要直接采用历史记录中的校验值。核实产物来源、构建提交、CPU 架构和独立可信的 SHA-256；两个本地文件哈希相同不等于来源可信。

```bash
python3 scripts/verge-service-core.py --release "$PWD/target/release/clash-rs"
```

默认服务文件为：

```text
/Library/Application Support/clash-verge-service/cores/verge-mihomo-alpha
```

脚本通过 macOS `proc_pidpath` 获取实际运行的 Clash/Mihomo 可执行文件路径及 PID，而不是从进程通用名称或应用目录推断。报告列出服务文件、发现的进程路径对应的磁盘文件、release 的 SHA-256、大小、属主和权限。路径不同也会分别列出。可用 `--core /绝对路径` 指定其他实际服务内核。

默认不执行任何内核。确认文件可信后，可在**普通用户**权限下增加 `--probe-versions`，分别调用 `-v`（5 秒超时）。禁止 root 执行这些二进制。权限拒绝、文件缺失、进程解析失败均在 JSON 中明确报告；核验命令退出 0 表示报告已生成，**不表示全部核验成功**。若只有文件读取需要管理员权限，可手动运行同一只读命令的 sudo 版本，勿带 `--probe-versions`。

报告的哈希与 `-v` 均来自**当前磁盘文件**，不能证明已有进程加载的是相同字节。替换后旧进程可能继续执行旧映像；必须在用户手动重启后重新核验路径、PID 与磁盘哈希，再结合服务 API/日志验证。发现不明进程或缺少权限时不能作“已部署”的结论。

## 2. 升级前复用现有隔离回归

以普通用户执行：

```bash
python3 scripts/test-verge-reload.py "$PWD/target/release/clash-rs"
```

该现有脚本在临时目录创建独立配置，明确关闭 TUN，通过 YAML/CLI 控制连接各重载五次并检查本地 fake-IP DNS。这里直接复用，不在升级工具中重写。它会启动隔离内核和本地监听端口，但不会验证真实服务副本、iWAN 登录或 TUN。

## 3. 手动停服后显式升级

自行记录当前 Verge 设置及已有网络状态；在 Verge 中手动停止目标内核/服务并保持停止，避免 GUI 自动拉起。工具不会替用户更改这些设置。再次只读核验确认目标 SHA-256。升级前审阅脚本，管理员执行期间不要让其他进程修改脚本或目标目录。

下列占位值必须替换为本次核实的完整 64 位十六进制哈希：

```bash
sudo python3 scripts/verge-service-core.py \
  --release "$PWD/target/release/clash-rs" \
  --install \
  --sha256 RELEASE_SHA256 \
  --expected-current-sha256 SERVICE_SHA256
```

工具在终端显示完整目标路径和新哈希，要求逐字输入 `REPLACE 路径 SHA256`；没有 `--yes`，不接受管道确认。不具备管理员权限时直接退出，不自行提权。

保护机制：

- 检查目标父目录及祖先属主/写权限，拒绝符号链接、特殊文件和特殊执行权限；拒绝不能完整保留的 macOS ACL/BSD file flags，需人工采用能保留元数据的升级方式。
- 使用同目录非阻塞排他锁；若目标进程仍在运行或 Clash/Mihomo 身份无法解析，拒绝升级。停服是用户责任，进程检查不能阻止外部程序在最后检查后重新拉起。
- 检查独立指定的当前/产物哈希，使用 `lipo -verify_arch` 检查本机架构；这不验证签名、产物来源或功能兼容性。
- 创建同目录、0700 权限的唯一备份目录，持久保存原文件和 `manifest.json`（原/新哈希、目标路径）。备份保留字节、uid/gid、mode 和扩展属性；属性复制失败即中止。
- 在备份目录暂存候选文件，再校验 SHA-256，复查停服和当前文件哈希，然后同文件系统原子替换。备份、清单、暂存文件和目录均 fsync。
- 替换后的校验/同步若抛出异常，会从原备份恢复；备份不会被消费。恢复本身失败时保留备份并报错。不会自动重启服务。

不保证断电、SIGKILL 或第二次故障下自动恢复；原子替换后的原文件及清单已持久保存，可用下面的显式回滚。备份路径在替换前就会打印，请保留。工具不自动删除备份。

## 4. 回滚与异常恢复

仍需用户手动停服；先读取当前目标哈希，再指定升级时打印的备份目录：

```bash
sudo python3 scripts/verge-service-core.py \
  --rollback '/Library/Application Support/clash-verge-service/cores/verge-mihomo-alpha.backup-实际后缀' \
  --expected-current-sha256 CURRENT_SERVICE_SHA256
```

回滚会验证清单目标和原文件哈希，再次要求逐字确认；同样先备份当前文件，然后原子恢复。若目标文件丢失、目录权限不安全、备份损坏或有 ACL/flags，工具拒绝操作；根据已保留的 `original`、清单和错误信息进行管理员人工恢复，不应绕过校验强行覆盖。

## 5. 尚需在用户 macOS 中验证

本次 Linux 开发环境未连接用户 Mac，不能判定当前是否仍运行旧版，也未执行实际管理员替换。

- 在目标 macOS 验证 `proc_pidpath`、权限拒绝诊断、普通用户版本探测、Mach-O 本机架构和实际服务路径；验证签名/Gatekeeper、扩展属性和服务目录权限策略。
- 用户停服后，验证管理员确认、备份、真实服务文件替换、显式回滚及 Verge 是否会自动覆盖服务副本。不要只检查应用目录。
- 用户手动重启服务，重新采集 PID/实际路径/哈希/API 版本和日志；确认 iWAN 不再出现旧 nonce 错误，并做真实 iWAN 请求与连续配置重载。
- **另行经用户确认**后，才在 Verge 内启用 TUN 并验证真实 IP、fake-IP、DNS、规则分流与重载；记录测试前后的系统代理、路由和 DNS，测试后恢复。离线回归通过不能替代这个端到端验证。

## 自动化测试

```bash
python3 -m unittest discover -s scripts/tests -v
python3 -m py_compile scripts/verge-service-core.py scripts/tests/test_verge_service_core.py scripts/test-verge-reload.py
git diff --check
```

测试仅在临时目录中写入合成文件，通过模拟进程和平台信息测试危险操作的拒绝路径，不访问实际服务目录，不启动内核，不更改网络。覆盖默认只读、确认拒绝、哈希错误、符号链接、特殊权限、并发锁、扩展属性、复制失败、替换后恢复、完整安装和回滚。CI 同时安排 Linux/macOS；本地通过不代表 macOS CI 或真实服务验证已完成。
