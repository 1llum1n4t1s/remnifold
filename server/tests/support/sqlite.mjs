let context;
let opened = false;
function bindStorage(ctx) {
  context = ctx;
  opened = false;
}
class DatabaseSync {
  constructor() {
    if (!context) throw Error("SQLite requires a test Durable Object");
    if (opened) throw Error("Concurrent database fixtures require distinct Durable Objects");
    opened = true;
    this.storage = context.storage;
  }
  exec(sql) {
    if (/^PRAGMA foreign_keys\s*=/i.test(sql)) return;
    return this.storage.sql.exec(sql).toArray();
  }
  prepare(sql) {
    const exec = (values) => this.storage.sql.exec(sql, ...values);
    return { get: (...v) => exec(v).toArray()[0], all: (...v) => exec(v).toArray(), run: (...v) => {
      exec(v).toArray();
      return { changes: this.storage.sql.exec("SELECT changes() AS n").one().n };
    } };
  }
  transaction(fn) {
    return this.storage.transactionSync(fn);
  }
  close() {
    this.storage.transactionSync(() => {
      this.storage.sql.exec("PRAGMA defer_foreign_keys = ON");
      for (const row of this.storage.sql.exec("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE '_cf_%'").toArray()) {
        try {
          this.storage.sql.exec(`DROP TABLE IF EXISTS "${row.name.replaceAll('"', '""')}"`);
        } catch (e) {
          if (!String(e).includes("no such table")) throw e;
        }
      }
    });
    opened = false;
  }
}
export {
  DatabaseSync,
  bindStorage
};
