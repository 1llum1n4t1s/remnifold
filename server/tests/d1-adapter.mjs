export function adapter(database) {
  const hooks = {};
  function prepare(sql) {
    const bind = (values = []) => ({ sql, values, bind: (...next) => bind(next),
      first: async () => { await hooks.first?.(sql, values); return database.prepare(sql).get(...values); },
      all: async () => { await hooks.all?.(sql, values); return { results: database.prepare(sql).all(...values) }; },
      run: async () => { await hooks.run?.(sql, values); return { meta: { changes: Number(database.prepare(sql).run(...values).changes) } }; },
      execute: () => { hooks.execute?.(sql, values); return /^\s*(SELECT|WITH|PRAGMA)\b/iu.test(sql)
        ? { results: database.prepare(sql).all(...values), meta: { changes: 0 } }
        : { results: [], meta: { changes: Number(database.prepare(sql).run(...values).changes) } }; },
    });
    return bind();
  }
  return { hooks, prepare, batch: async (operations) => {
    await hooks.batch?.(operations);
    database.exec("BEGIN");
    try { const results = operations.map((operation) => operation.execute()); database.exec("COMMIT"); return results; }
    catch (error) { database.exec("ROLLBACK"); throw error; }
  } };
}
