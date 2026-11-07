# 贡献指南

欢迎提交问题和改进建议。项目目前处于提炼核心能力的早期阶段，较大的架构改动建议先通过 issue 讨论。

## 开发环境

- Rust 1.88 或更新版本
- Node.js 22.15 或更新版本及 npm（前端、插件 SDK 和 provider 适配器）
- Docker（构建镜像和运行默认镜像 smoke check）
- Docker Compose（按 Compose 方式运行应用时需要）

~~~sh
npm ci
npm run dev
npm run check
npm audit --audit-level=high
npm test
npm run check:licenses
npm run check:plugin-sdk
npm run check:public-boundary
npm run check:git-history
npm run build
~~~

`npm test` 运行 Node provider adapter 与插件 SDK 的测试。Rust 服务测试使用 `cargo test --workspace --locked`。

运行面板开发服务器前，在另一个终端启动后端：

~~~sh
npm start
~~~

Vite 会把 `/api` 请求代理到 `http://127.0.0.1:8080`。Rust 服务提供应用 API、数据仓库、追更工作流、插件运行时和可选 MCP；Node 代码用于前端工具、插件 SDK 与作为 Rust 子进程运行的 provider adapter。API 和插件契约尚未承诺稳定。

## 代码边界

- 新增或修改 provider 前先阅读 [docs/PROVIDERS.md](docs/PROVIDERS.md) 和 [Rust plugin protocol](docs/RUST-PLUGIN-PROTOCOL.md)，按当前 API v2 契约定义能力、配置和失败语义。该接口仍为 0.x，不承诺稳定。
- 核心模块负责节目状态、缺集判断、事件状态和工作流顺序。
- 插件负责外部服务调用和统一数据转换，不直接改写追更状态。
- 插件新增能力应声明在契约中，并说明配置、错误分类、重置和健康检查行为。
- 外部数据进入核心前应校验结构，并限制错误信息和日志中的敏感数据。
- 涉及文件写入、移动或删除时，先定义明确的预览和恢复语义。
- 本地存储插件是文件系统 provider 的参考实现；源目录只读，目标路径限制在配置根目录内。
- 新 provider 应说明文件 ID 稳定性、完整清单语义、路径边界和删除模式。
- 提交前运行 `cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo check --workspace --all-targets --locked`、`npm run check`、`npm run check:plugin-sdk`、`npm audit --audit-level=high`、`npm test`、`npm run check:licenses`、`npm run check:public-boundary`、`npm run check:git-history` 和 `npm run build`。
- 依赖升级后先核对上游许可证和归属，再运行 `npm run update:license-inventory` 更新锁文件许可证基线并复核差异。

## 提交变更

- 描述用户可见的行为和涉及的模块。
- 不提交密钥、Cookie、Webhook、数据库、运行日志、媒体文件或真实网盘清单。
- 依赖升级需说明原因；不要为了消除安全提示而强制升级不兼容的大版本。
- 有行为变化时，为关键决策和失败恢复路径补充覆盖。
