// SQLとトランザクションを最初のawaitより前に実行するD1互換adapter。
export function adapter(database) {
  function prepare(sql) {
    const bind = (values = []) => ({ sql, values, bind: (...next) => bind(next),
      first: async () => database.prepare(sql).get(...values),
      all: async () => ({ results: database.prepare(sql).all(...values) }),
      run: async () => ({ meta: { changes: Number(database.prepare(sql).run(...values).changes) } }),
      execute: () => /^\s*(SELECT|WITH|PRAGMA)\b/iu.test(sql)
        ? { results: database.prepare(sql).all(...values), meta: { changes: 0 } }
        : { results: [], meta: { changes: Number(database.prepare(sql).run(...values).changes) } },
    });
    return bind();
  }
  return { prepare, batch: async (operations) => {
    return database.transaction(() => operations.map(operation => operation.execute()));
  } };
}
