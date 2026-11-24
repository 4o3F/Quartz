-- Closed preset codes: 1 rest. Presets are valid only for kind 5 (note).
ALTER TABLE annotations ADD COLUMN label_preset INTEGER
  CHECK (label_preset IS NULL OR (kind = 5 AND label_preset = 1));

INSERT INTO enum_labels (enum_name, code, label)
VALUES ('annotations.label_preset', 1, 'rest');

UPDATE annotations
SET label_preset = 1, label = NULL
WHERE kind = 5 AND label = '休息' AND note IS NULL;

DROP VIEW annotations_v;
CREATE VIEW annotations_v AS
SELECT t.*,
       e_kind.label AS kind_label,
       e_label_preset.label AS label_preset_label,
       strftime('%Y-%m-%d %H:%M:%f', t.start_utc_ms / 1000.0, 'unixepoch', 'localtime') AS start_local,
       strftime('%Y-%m-%d %H:%M:%f', t.end_utc_ms / 1000.0, 'unixepoch', 'localtime') AS end_local,
       strftime('%Y-%m-%d %H:%M:%f', t.created_utc_ms / 1000.0, 'unixepoch', 'localtime') AS created_local,
       strftime('%Y-%m-%d %H:%M:%f', t.updated_utc_ms / 1000.0, 'unixepoch', 'localtime') AS updated_local
FROM annotations t
LEFT JOIN enum_labels e_kind ON e_kind.enum_name = 'annotations.kind' AND e_kind.code = t.kind
LEFT JOIN enum_labels e_label_preset ON e_label_preset.enum_name = 'annotations.label_preset' AND e_label_preset.code = t.label_preset;
