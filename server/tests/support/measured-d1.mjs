export const utf8Bytes = (value) => new TextEncoder().encode(value).length;

export async function bounded(promise, description) {
  let timer;
  try {
    return await Promise.race([promise, new Promise((_, reject) => { timer = setTimeout(() => reject(new Error(`${description} exceeded 10 seconds`)), 10_000); })]);
  } finally { clearTimeout(timer); }
}

// D1の返却直前に、実SELECTのJSONサイズとembedding取得数を測る。WireサイズやD1課金の推定ではない。
// keepRecords を指定した場合だけ、返した行そのものを reads[].records に残す。
export function measuredD1(db, { keepRecords = false } = {}) {
  const reads = [];
  let beforeRead;
  let beforeBatch;
  function prepare(sql) {
    const bind = (values = []) => {
      const select = () => {
        beforeRead?.(sql, values);
        const rows = db.prepare(sql).all(...values);
        reads.push({ sql, rows: rows.length, rowBytes: rows.reduce((sum, row) => sum + utf8Bytes(JSON.stringify(row)), 0),
          ...(keepRecords ? { records: rows } : {}), embeddings: rows.filter((row) => typeof row.embedding_json === "string").length });
        return rows;
      };
      return { sql, values, bind: (...next) => bind(next),
        first: async () => select()[0], all: async () => ({ results: select() }),
        run: async () => ({ meta: { changes: Number(db.prepare(sql).run(...values).changes) } }),
        execute: () => /^\s*(SELECT|WITH|PRAGMA)\b/iu.test(sql)
          ? { results: select(), meta: { changes: 0 } }
          : { results: [], meta: { changes: Number(db.prepare(sql).run(...values).changes) } },
      };
    };
    return bind();
  }
  return { reads, prepare, setBeforeRead(callback) { beforeRead = callback; }, beforeBatch(callback) { beforeBatch = callback; },
    batch: async (operations) => {
      const callback = beforeBatch; beforeBatch = undefined; callback?.();
      return db.transaction(() => operations.map((operation) => operation.execute()));
    } };
}
