# only js

构建命令行应用程序`oj`,包含以下子命令

## server
  执行 `oj server -c config.yaml` 将读取配置文件，启动一个web服务

  -c config.yaml 读取配置文件中的信息，启动服务，其中包含 host port db redis 等配置信息
  
  以下为一个例子
  ```yaml
    server:
      host: "localhost"
      port: 778
    db:
      default: "sqlite:///tmp/database.db"
      mysql: "mysql:host=localhost;dbname=test"
      pg: "postgresql://dbuser:pwd@localhost/testdb"
    redis:
      default: "redis://user:password@127.0.0.1:6379/1"
      other: "redis://user:password@127.0.0.1:6379/2"
  ```

  -b base 作为默认的服务基础路由，默认 /v1/api
   
  -d dir 以 `dir` 作为项目的服务目录，一个开发中(--dev)的服务目录如下
  ```
    dist   # 编译目录与src结构相同
    src    # 源码目录，首层子目录为模块名
    ├── moduleA
    │   └── featA # 子特性文件夹
    │   └── featB
    │   └── manifest.yaml # moduleA 的清单文件
    └── user
        ├── account
        └── profile
        └── manifest.yaml # user 模块的清单
  ```
   每个模块目录下包含一个配置文件 manifest.yaml 用于记录该模型的相关清单，其案例结构如下
  ```yaml
    name: "user" # 与模块名文件夹同名，即父目录名，约束条件
    desc: "用户信息相关，记录账号、地址等个人信息" # 模块的简短描述
    version: "0.1.0" # 当前的版本，打包的时候加入打包路径
    config: # 本模块的其他设置信息
  ```
### 访问规则
   --dev 开发模式下
   如果某个子文件夹中存在 `api.ts`, 如 `src/moduleA/featB/api.ts`, 则可以通过 `GET` `/v1/api/moduleA/featB/` 来执行 `api.ts` 中导出的`get`方法
   --release 发布模式下，一般是编译文件目录，如 `-d dist/` 下存在文件 `moduleA/featB/api.js`, 则可以通过 `GET` `/v1/api/moduleA/featB/` 来执行 `api.js` 中导出的`get`方法
   其中 `DELETE` 映射为 `del` 方法

   `api.ts` 伪代码如下
  ```ts
    import 'xxx/xxxxx/xx' // 导入需要模块
    function get(){
        let r = db.query("select * from user where id = ?", http.param("id",0))
        json.ok(r)
    }
    function post(){}
    // 
    export default {get,post}
  ```
   以上代码通过包装 `oj build moduleA` 命令来编译，制品存放在 `dist`,`build`子命令为 `vite` 工具的包装。

---

## 实现记录（2026-08-22，commit `587ad16..61ee89e`）

**状态：** v0.1 已实现，`oj server` + user/order sample 验收通过（debug/release 双绿）。

- **子命令**：`server`（`-c/-b/-d/--dev`）已可用；`build` 与无命令仍为占位（v0.1 范围内）。
- **路由镜像**：任意深度目录 → `api.ts`（dev）/ `api.js`（release）；动词全表（`DELETE→del`）；越界/穿越段按 404。
- **执行模型**：deno_ast 转译（mtime 全局缓存）+ deno_core side-module 驱动（TLA driver `import` api 模块 → `default[method]`）。
- **偏差**（见 spec 收官注记）：
  - vendored `escape-goat` 替代 spec 原 `nanoid`（裸 deno_core 无 `crypto.getRandomValues`）。
  - sample 端口 778 → 9778（macOS 特权端口）。
  - `-d` 路径相对 CWD 而非 config_dir。
- **测试**：debug 61 passed + 1 ignored；release 61 passed + 1 ignored（root/server/oj，含 E2E UC-1..15）。
