import { positiveId, requireRole } from "../_shared/validate";

function get(): void {
  const id = Number(http.param("id", 0));
  const rows = id > 0
    ? db.query("select id, name, role from account where id = ?", [id])
    : db.query("select id, name, role from account", []);
  rows.then((r) => json.ok(r)).catch((e) => json.fail(500, String(e)));
}

function post(): void {
  const b = http.body as { name?: string; role?: string };
  if (!b.name) { json.fail(400, "name required"); return; }
  const role = (() => { try { return requireRole(b.role ?? "user"); } catch (e) { return ""; } })();
  if (!role) { json.fail(400, "role must be admin|user"); return; }
  db.exec("insert into account (name, role) values (?, ?)", [b.name, role])
    .then(() => json.ok({ created: true }))
    .catch((e) => json.fail(500, String(e)));
}

function put(): void {
  const b = http.body as { id?: number; name?: string };
  const id = (() => { try { return positiveId(b.id); } catch { return 0; } })();
  if (!id || !b.name) { json.fail(400, "id and name required"); return; }
  db.exec("update account set name = ? where id = ?", [b.name, id])
    .then(() => json.ok({ updated: true }))
    .catch((e) => json.fail(500, String(e)));
}

function del(): void {
  const id = positiveId(http.param("id", 0));
  db.exec("delete from account where id = ?", [id])
    .then(() => json.ok({ deleted: true }))
    .catch((e) => json.fail(500, String(e)));
}

function patch(): void {
  const b = http.body as { id?: number; role?: string };
  const role = requireRole(b.role);
  db.exec("update account set role = ? where id = ?", [role, positiveId(b.id)])
    .then(() => json.ok({ patched: true }))
    .catch((e) => json.fail(500, String(e)));
}

function head(): void { get(); }

function options(): void {
  json.ok({ methods: ["get", "post", "put", "del", "patch", "head", "options"] });
}

export default { get, post, put, del, patch, head, options };
