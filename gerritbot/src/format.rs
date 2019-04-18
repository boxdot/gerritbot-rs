use itertools::Itertools as _;

use rlua::{FromLua as _, Function as LuaFunction, Lua, Value as LuaValue};
use serde::Serialize;
use serde_json::Value as JsonValue;

use gerritbot_gerrit as gerrit;

const UNKNOWN_USER: &str = "<unknown user>";
const DEFAULT_FORMAT_SCRIPT: &str = include_str!("../../scripts/format.lua");

pub struct Formatter {
    lua: Lua,
}

impl Default for Formatter {
    fn default() -> Self {
        Self {
            lua: load_format_script(DEFAULT_FORMAT_SCRIPT).unwrap(),
        }
    }
}

fn load_format_script(script_source: &str) -> Result<Lua, String> {
    let lua = Lua::new();
    lua.context(|context| -> Result<(), String> {
        let globals = context.globals();
        context
            .load(script_source)
            .exec()
            .map_err(|err| format!("syntax error: {}", err))?;
        let _: LuaFunction = globals
            .get("format_approval")
            .map_err(|_| "format_approval function missing")?;
        Ok(())
    })?;
    Ok(lua)
}

fn json_to_lua<'lua>(json: &JsonValue, lua: rlua::Context<'lua>) -> rlua::Result<LuaValue<'lua>> {
    Ok(match json {
        JsonValue::Null => LuaValue::Nil,
        JsonValue::Bool(b) => LuaValue::Boolean(*b),
        JsonValue::Number(n) => {
            if let Some(n) = n.as_i64() {
                LuaValue::Integer(n)
            } else if let Some(n) = n.as_f64() {
                LuaValue::Number(n)
            } else {
                Err(rlua::Error::ToLuaConversionError {
                    from: "serde_json::Number",
                    to: "Integer",
                    message: Some(format!("value {} too large", n)),
                })?
            }
        }
        JsonValue::String(s) => lua.create_string(s).map(LuaValue::String)?,
        JsonValue::Array(values) => {
            let table = lua.create_table()?;

            for (i, value) in values.iter().enumerate() {
                table.set(LuaValue::Integer(i as i64 + 1), json_to_lua(value, lua)?)?;
            }

            LuaValue::Table(table)
        }
        JsonValue::Object(items) => {
            let table = lua.create_table()?;

            for (key, value) in items {
                let key = lua.create_string(key)?;
                let value = json_to_lua(value, lua)?;
                table.set(key, value)?;
            }

            LuaValue::Table(table)
        }
    })
}

fn to_lua_via_json<'lua, T: Serialize>(
    value: T,
    lua: rlua::Context<'lua>,
) -> Result<LuaValue<'lua>, Box<dyn std::error::Error>> {
    let json_value = serde_json::to_value(value)?;
    let lua_value = json_to_lua(&json_value, lua)?;
    Ok(lua_value)
}

impl Formatter {
    pub fn format_approval(
        &self,
        event: &gerrit::CommentAddedEvent,
        approval: &gerrit::Approval,
        is_human: bool,
    ) -> Result<Option<String>, String> {
        self.lua
            .context(|context| -> Result<Option<String>, String> {
                let globals = context.globals();

                let lua_format_approval: LuaFunction = globals
                    .get("format_approval")
                    .map_err(|_| "format_approval function missing".to_string())?;
                let lua_event = to_lua_via_json(event, context)
                    .map_err(|e| format!("failed to serialize event: {}", e))?;
                let lua_approval = to_lua_via_json(approval, context)
                    .map_err(|e| format!("failed to serialize approval: {}", e))?;
                let lua_result = lua_format_approval
                    .call::<_, LuaValue>((lua_event, lua_approval, is_human))
                    .map_err(|err| format!("lua formatting function failed: {}", err))?;

                match lua_result {
                    LuaValue::Nil => Ok(None),
                    _ => Ok(Some(String::from_lua(lua_result, context).map_err(
                        |e| format!("failed to convert formatting result to string: {}", e),
                    )?)),
                }
            })
    }

    pub fn format_comment_added(
        &self,
        event: &gerrit::CommentAddedEvent,
        is_human: bool,
    ) -> Result<Option<String>, String> {
        let messages: Vec<String> = event
            .approvals
            .iter()
            .filter(|approval| {
                approval.old_value.as_ref() != Some(&approval.value) && approval.value != "0"
            })
            .filter_map(|approval| self.format_approval(event, approval, is_human).transpose())
            .collect::<Result<_, _>>()?;

        let inline_comments = self.format_inline_comments(&event.change, &event.patchset);

        let message = if !messages.is_empty() {
            // We got some approvals
            messages.join("\n\n")
        } else if is_human && inline_comments.is_some() {
            // We did not get any approvals, but we got inline comments from a human.
            event.comment.clone()
        } else {
            return Ok(None);
        };

        Ok(Some(
            inline_comments
                .map(|c| format!("{}\n\n{}", message, c))
                .unwrap_or(message),
        ))
    }

    pub fn format_reviewer_added(
        &self,
        event: &gerrit::ReviewerAddedEvent,
    ) -> Result<String, String> {
        Ok(format!(
            "[{}]({}) ({}) 👓 Added as reviewer",
            event.change.subject,
            event.change.url,
            event
                .change
                .owner
                .username
                .as_ref()
                .map(String::as_str)
                .unwrap_or(UNKNOWN_USER)
        ))
    }

    fn format_inline_comments(
        &self,
        change: &gerrit::Change,
        patchset: &gerrit::Patchset,
    ) -> Option<String> {
        let change_number = change.number;
        let base_url = {
            let last_slash = change.url.rfind('/').unwrap();
            &change.url[..last_slash]
        };
        let patch_set_number = patchset.number;

        patchset
            .comments
            .as_ref()
            .cloned()
            .map(|mut comments| {
                comments.sort_by(|a, b| a.file.cmp(&b.file));
                comments
                    .into_iter()
                    .group_by(|c| c.file.clone())
                    .into_iter()
                    .map(|(file, comments)| -> String {
                        let line_comments = comments
                            .map(|comment| {
                                let mut lines = comment.message.split('\n');
                                let url = format!(
                                    "{}/#/c/{}/{}/{}@{}",
                                    base_url,
                                    change_number,
                                    patch_set_number,
                                    comment.file,
                                    comment.line
                                );
                                let first_line = lines.next().unwrap_or("");
                                let first_line = format!(
                                    "> [Line {}]({}) by {}: {}",
                                    comment.line,
                                    url,
                                    comment
                                        .reviewer
                                        .username
                                        .as_ref()
                                        .map(String::as_str)
                                        .unwrap_or(UNKNOWN_USER),
                                    first_line
                                );
                                let tail = lines
                                    .map(|l| format!("> {}", l))
                                    .intersperse("\n".into())
                                    .collect::<Vec<_>>()
                                    .concat();
                                format!("{}\n{}", first_line, tail)
                            })
                            .intersperse("\n".into())
                            .collect::<Vec<_>>()
                            .concat();
                        format!("`{}`\n\n{}", file, line_comments)
                    })
                    .intersperse("\n\n".into())
                    .collect::<Vec<_>>()
                    .concat()
            })
            .filter(|s| !s.is_empty())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    const EVENT_JSON : &'static str = r#"
{"author":{"name":"Approver","username":"approver","email":"approver@approvers.com"},"approvals":[{"type":"Code-Review","description":"Code-Review","value":"2","oldValue":"-1"}],"comment":"Patch Set 1: Code-Review+2\n\nJust a buggy script. FAILURE\n\nAnd more problems. FAILURE","patchSet":{"number":1,"revision":"49a65998c02eda928559f2d0b586c20bc8e37b10","parents":["fb1909b4eda306985d2bbce769310e5a50a98cf5"],"ref":"refs/changes/42/42/1","uploader":{"name":"Author","email":"author@example.com","username":"Author"},"createdOn":1494165142,"author":{"name":"Author","email":"author@example.com","username":"Author"},"isDraft":false,"kind":"REWORK","sizeInsertions":0,"sizeDeletions":0},"change":{"project":"demo-project","branch":"master","id":"Ic160fa37fca005fec17a2434aadf0d9dcfbb7b14","number":49,"subject":"Some review.","owner":{"name":"Author","email":"author@example.com","username":"author"},"url":"http://localhost/42","commitMessage":"Some review.\n\nChange-Id: Ic160fa37fca005fec17a2434aadf0d9dcfbb7b14\n","status":"NEW"},"project":"demo-project","refName":"refs/heads/master","changeKey":{"id":"Ic160fa37fca005fec17a2434aadf0d9dcfbb7b14"},"type":"comment-added","eventCreatedOn":1499190282}"#;

    const CHANGE_JSON_WITH_COMMENTS : &'static str = r#"
{"project":"gerritbot-rs","branch":"master","id":"If70442f674c595a59f3e44280570e760ba3584c4","number":1,"subject":"Bump version to 0.6.0","owner":{"name":"Administrator","email":"admin@example.com","username":"admin"},"url":"http://localhost:8080/1","commitMessage":"Bump version to 0.6.0\n\nChange-Id: If70442f674c595a59f3e44280570e760ba3584c4\n","createdOn":1524584729,"lastUpdated":1524584975,"open":true,"status":"NEW","comments":[{"timestamp":1524584729,"reviewer":{"name":"Administrator","email":"admin@example.com","username":"admin"},"message":"Uploaded patch set 1."},{"timestamp":1524584975,"reviewer":{"name":"jdoe","email":"john.doe@localhost","username":"jdoe"},"message":"Patch Set 1:\n\n(1 comment)"}]}"#;

    const PATCHSET_JSON_WITH_COMMENTS : &'static str = r#"{"number":1,"revision":"3f58af760fc1e39fcc4a85b8ab6a6be032cf2ae2","parents":["578bc1e684098d2ac597e030442c3472f15ac3ad"],"ref":"refs/changes/01/1/1","uploader":{"name":"Administrator","email":"admin@example.com","username":"admin"},"createdOn":1524584729,"author":{"name":"jdoe","email":"jdoe@example.com","username":""},"isDraft":false,"kind":"REWORK","comments":[{"file":"/COMMIT_MSG","line":1,"reviewer":{"name":"jdoe","email":"john.doe@localhost","username":"jdoe"},"message":"This is a multiline\ncomment\non some change."}],"sizeInsertions":2,"sizeDeletions":-2}"#;

    fn get_event() -> gerrit::CommentAddedEvent {
        let event: Result<gerrit::Event, _> = serde_json::from_str(EVENT_JSON);
        match event.expect("failed to decode event") {
            gerrit::Event::CommentAdded(event) => event,
            event => panic!("wrong type of event: {:?}", event),
        }
    }

    fn get_change_with_comments() -> (gerrit::Change, gerrit::Patchset) {
        let change: Result<gerrit::Change, _> = serde_json::from_str(CHANGE_JSON_WITH_COMMENTS);
        assert!(change.is_ok());
        let patchset: Result<gerrit::Patchset, _> =
            serde_json::from_str(PATCHSET_JSON_WITH_COMMENTS);
        assert!(patchset.is_ok());
        (change.unwrap(), patchset.unwrap())
    }

    #[test]
    fn test_format_approval() {
        let event = get_event();
        let res = Formatter::default().format_approval(&event, &event.approvals[0], true);
        assert_eq!(
            res,
            Ok(Some("[Some review.](http://localhost/42) (demo-project) 👍 +2 (Code-Review) from approver\n\n> Just a buggy script. FAILURE<br>\n> And more problems. FAILURE".to_string()))
        );
    }

    #[test]
    fn format_approval_filters_specific_messages() {
        let mut event = get_event();
        event.approvals[0].approval_type = String::from("Some new type");
        let res = Formatter::default().format_approval(&event, &event.approvals[0], true);
        assert_eq!(res, Ok(None));
    }

    #[test]
    fn test_format_comments() {
        let (change, mut patchset) = get_change_with_comments();
        patchset.comments = None;
        assert_eq!(
            Formatter::default().format_inline_comments(&change, &patchset),
            None
        );

        let (change, patchset) = get_change_with_comments();
        assert_eq!(Formatter::default().format_inline_comments(&change, &patchset),
                   Some("`/COMMIT_MSG`\n\n> [Line 1](http://localhost:8080/#/c/1/1//COMMIT_MSG@1) by jdoe: This is a multiline\n> comment\n> on some change.".into()));
    }
}
