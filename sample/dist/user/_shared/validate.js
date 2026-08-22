// 模块内共享校验（无 api.ts → 非路由目录，UC-13 相对导入载体）
export function requireRole(role) {
  if (role !== "admin" && role !== "user") throw new Error("invalid role: " + role);
  return role;
}

export function positiveId(raw) {
  const n = Number(raw);
  if (!Number.isInteger(n) || n <= 0) throw new Error("invalid id: " + String(raw));
  return n;
}
