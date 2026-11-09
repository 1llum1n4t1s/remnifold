-- 旧24時間待ちを、元の延期時刻から1時間の待ちへ移す。
-- 継続中の読み取り、期限到来済みの順序、本文と私的進捗は変更しない。
UPDATE memory_organization_progress
SET next_attempt=strftime('%Y-%m-%dT%H:%M:%fZ',next_attempt,'-23 hours')
WHERE next_offset=0 AND next_attempt>strftime('%Y-%m-%dT%H:%M:%fZ','now');

UPDATE memory_organization_groups
SET next_attempt=strftime('%Y-%m-%dT%H:%M:%fZ',next_attempt,'-23 hours')
WHERE status='deferred' AND next_attempt>strftime('%Y-%m-%dT%H:%M:%fZ','now');
