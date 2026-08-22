# oj sample — user/order

  cargo run -p oj -- server -c sample/config.yaml -d sample/src --dev   # dev（TS，热重载）
  curl http://localhost:9778/v1/api/user/account/?id=1

  cargo run -p oj -- server -c sample/config.yaml -d sample/dist       # release（dist 手写制品）

- 路由 = 目录镜像：src/user/profile/detail/api.ts → /v1/api/user/profile/detail/
- node_modules/escape-goat 为直接 vendor 的纯 ESM 包（可 npm install 替换）
- db.sqlite 由 seed.sql 初始化（幂等），已 gitignore
