import { requireRole } from "../../user/_shared/validate.js";

function get() {
  const role = requireRole(http.param("role", "admin")); // 跨模块相对导入（UC-13）
  db.query(
    `select o.id, o.no, o.amount, a.name as account_name, a.role
     from orders o join account a on a.id = o.account_id
     where a.role = ? order by o.id`,
    [role],
  )
    .then((r) => json.ok(r))
    .catch((e) => json.fail(500, String(e)));
}

export default { get };
