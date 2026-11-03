import test,{assert} from './test.mjs';
import {DatabaseSync} from './sqlite.mjs';
import {applyMigrations} from './migrations.mjs';
test('actual SQLite enforces migration foreign keys, cascades and transaction rollback',()=>{
 const db=new DatabaseSync(':memory:');
 try{
  applyMigrations(db);
  assert.equal(db.prepare('PRAGMA foreign_keys').get().foreign_keys,1);
  assert.throws(()=>db.prepare("INSERT INTO memories(id,custom_id,container_tag,content,created_at,updated_at) VALUES('invalid','invalid','absent','invalid','2026-01-01','2026-01-01')").run(),/FOREIGN KEY/iu);
  assert.equal(db.prepare("SELECT COUNT(*) AS n FROM memories WHERE id='invalid'").get().n,0);
  db.exec('CREATE TABLE fk_parent(id TEXT PRIMARY KEY); CREATE TABLE fk_child(id TEXT PRIMARY KEY,parent_id TEXT NOT NULL REFERENCES fk_parent(id) ON DELETE CASCADE);');
  assert.throws(()=>db.prepare("INSERT INTO fk_child VALUES('orphan','missing')").run(),/FOREIGN KEY/iu);
  db.prepare("INSERT INTO fk_parent VALUES('parent')").run();db.prepare("INSERT INTO fk_child VALUES('child','parent')").run();
  db.prepare("DELETE FROM fk_parent WHERE id='parent'").run();assert.equal(db.prepare('SELECT COUNT(*) AS n FROM fk_child').get().n,0);
  assert.throws(()=>db.transaction(()=>{db.prepare("INSERT INTO fk_parent VALUES('rollback')").run();db.prepare("INSERT INTO fk_child VALUES('rollback-child','rollback')").run();throw Error('rollback-canary')}),/rollback-canary/);
  assert.equal(db.prepare('SELECT COUNT(*) AS n FROM fk_parent').get().n,0);assert.equal(db.prepare('SELECT COUNT(*) AS n FROM fk_child').get().n,0);
 }finally{db.close()}
});
