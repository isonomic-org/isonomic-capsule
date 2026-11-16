BEGIN;
CREATE TABLE blobs (
                handle TEXT PRIMARY KEY,
                owner TEXT,
                content_type TEXT NOT NULL,
                size INTEGER NOT NULL,
                written INTEGER NOT NULL
            );
CREATE TABLE grants (
                app TEXT PRIMARY KEY,
                manifest TEXT NOT NULL
            );
INSERT INTO "grants" VALUES ('com.isonomic.chat', '{"id":"com.isonomic.chat","name":"Chat","version":"0.1.0","icon":"sparkle","capabilities":{"agents":{}}}');
CREATE TABLE session_events (
                session TEXT NOT NULL,
                seq INTEGER NOT NULL,
                schema INTEGER NOT NULL,
                payload TEXT NOT NULL,
                PRIMARY KEY (session, seq)
            ) WITHOUT ROWID;
INSERT INTO "session_events" VALUES ('ses_1794794815553_0001', 1, 1, '{"session":"ses_1794794815553_0001","seq":1,"at":1794794815553,"type":"session.start","class":"interactive","app":"com.isonomic.chat","principal":{"kind":"user"},"preset":{"id":"chat","version":3},"profile":"standard"}');
INSERT INTO "session_events" VALUES ('ses_1794794815553_0001', 2, 1, '{"session":"ses_1794794815553_0001","seq":2,"at":1794794815553,"type":"system","origin":"preset","text":"You are Isonomic, an assistant that runs on the person''s own device. Answer directly and accurately. When a request could be read more than one way, take the most likely reading and answer it. Say so when you are unsure of a fact. Keep answers short: lead with the answer and use at most a few short paragraphs or a short list, unless the person asks for more detail."}');
INSERT INTO "session_events" VALUES ('ses_1794794815553_0001', 3, 1, '{"session":"ses_1794794815553_0001","seq":3,"at":1794794815557,"turn":"trn_0001","type":"turn.start","cause":"user"}');
INSERT INTO "session_events" VALUES ('ses_1794794815553_0001', 4, 1, '{"session":"ses_1794794815553_0001","seq":4,"at":1794794815557,"turn":"trn_0001","type":"user","content":[{"type":"text","text":"Say hello"}]}');
INSERT INTO "session_events" VALUES ('ses_1794794815553_0001', 5, 1, '{"session":"ses_1794794815553_0001","seq":5,"at":1794794815557,"turn":"trn_0001","type":"system","origin":"context","text":"Current date: Monday, 16 November 2026 (UTC). Language: en.","context":{"date":"2026-11-16","time_zone":"UTC","language":"en"}}');
INSERT INTO "session_events" VALUES ('ses_1794794815553_0001', 6, 1, '{"session":"ses_1794794815553_0001","seq":6,"at":1794794815557,"turn":"trn_0001","step":"stp_0001","type":"step.start","purpose":"reply","model":"local:default","params":{"max_output_tokens":1024,"reasoning":"low"}}');
INSERT INTO "session_events" VALUES ('ses_1794794815553_0001', 7, 1, '{"session":"ses_1794794815553_0001","seq":7,"at":1794794815562,"turn":"trn_0001","step":"stp_0001","type":"assistant","content":[{"type":"text","text":"Hello world"}],"stop":"end","stream":[[0,5,5],[0,6,5]],"usage":{"input_tokens":0,"output_tokens":2}}');
INSERT INTO "session_events" VALUES ('ses_1794794815553_0001', 8, 1, '{"session":"ses_1794794815553_0001","seq":8,"at":1794794815562,"turn":"trn_0001","step":"stp_0001","type":"step.end","outcome":"complete","duration_ms":5,"usage":{"input_tokens":0,"output_tokens":2}}');
INSERT INTO "session_events" VALUES ('ses_1794794815553_0001', 9, 1, '{"session":"ses_1794794815553_0001","seq":9,"at":1794794815562,"turn":"trn_0001","type":"turn.end","stop":"done","steps":1,"duration_ms":5,"usage":{"input_tokens":0,"output_tokens":2}}');
INSERT INTO "session_events" VALUES ('ses_1794794815553_0001', 10, 1, '{"session":"ses_1794794815553_0001","seq":10,"at":1794794815562,"turn":"trn_0001","step":"stp_0002","type":"step.start","purpose":"title","model":"local:default","params":{"max_output_tokens":64,"reasoning":"off"}}');
INSERT INTO "session_events" VALUES ('ses_1794794815553_0001', 11, 1, '{"session":"ses_1794794815553_0001","seq":11,"at":1794794815566,"turn":"trn_0001","step":"stp_0002","type":"assistant","content":[{"type":"text","text":"Hello world"}],"stop":"end","stream":[[0,5,4],[0,6,4]],"usage":{"input_tokens":0,"output_tokens":2}}');
INSERT INTO "session_events" VALUES ('ses_1794794815553_0001', 12, 1, '{"session":"ses_1794794815553_0001","seq":12,"at":1794794815566,"turn":"trn_0001","step":"stp_0002","type":"step.end","outcome":"complete","duration_ms":4,"usage":{"input_tokens":0,"output_tokens":2}}');
INSERT INTO "session_events" VALUES ('ses_1794794815553_0001', 13, 1, '{"session":"ses_1794794815553_0001","seq":13,"at":1794794815566,"turn":"trn_0001","type":"title","title":"Hello world","source":"model"}');
INSERT INTO "session_events" VALUES ('ses_1794794815553_0001', 14, 1, '{"session":"ses_1794794815553_0001","seq":14,"at":1794794815566,"type":"title","title":"Lisbon plan","source":"user"}');
CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                app TEXT NOT NULL,
                meta TEXT NOT NULL
            );
INSERT INTO "sessions" VALUES ('ses_1794794815553_0001', 'com.isonomic.chat', '{"id":"ses_1794794815553_0001","app":"com.isonomic.chat","class":"interactive","preset":{"id":"chat","version":3},"title":"Lisbon plan","status":"active","archived":false,"forked_from":null,"created_at":1794794815553,"updated_at":1794794815566,"model":null}');
CREATE TABLE tokens (
                digest TEXT PRIMARY KEY,
                app TEXT NOT NULL REFERENCES grants(app)
            );
INSERT INTO "tokens" VALUES ('bd083704ef60d63d6d4640011e750e8dd4b2120d3d22753cdab1e4204b0f8e30', 'com.isonomic.chat');
CREATE INDEX sessions_by_app ON sessions (app);
COMMIT;
