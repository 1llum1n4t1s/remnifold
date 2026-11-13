-- 手動の記憶整理で「直近N日分」を選ぶとき、作成日時の範囲検索を索引で絞る。
-- 追加のみの索引で、既存の記憶・ID・整理状態は変更しない。
CREATE INDEX IF NOT EXISTS idx_memories_created ON memories(created_at,id);
