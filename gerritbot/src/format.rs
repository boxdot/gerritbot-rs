use rlua::{FromLua as _, Function as LuaFunction, Lua, StdLib as LuaStdLib, Value as LuaValue};
use serde::Serialize;
use serde_json::Value as JsonValue;

use gerritbot_gerrit as gerrit;

const UNKNOWN_USER: &str = "<unknown user>";
const DEFAULT_FORMAT_SCRIPT: &str = include_str!("../../scripts/format.lua");
const LUA_FORMAT_COMMENT_ADDED: &str = "format_comment_added";
const LUA_FORMAT_FUNCTIONS: &[&str] = &[LUA_FORMAT_COMMENT_ADDED];

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
    let lua_std_lib = LuaStdLib::BASE | LuaStdLib::STRING | LuaStdLib::TABLE;
    let lua = Lua::new_with(lua_std_lib);
    lua.context(|context| -> Result<(), String> {
        let globals = context.globals();
        context
            .load(script_source)
            .exec()
            .map_err(|err| format!("syntax error: {}", err))?;

        // check that the required functions are present
        for function_name in LUA_FORMAT_FUNCTIONS {
            let _: LuaFunction = globals
                .get(*function_name)
                .map_err(|_| format!("{} function missing", function_name))?;
        }

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
    pub fn format_comment_added(
        &self,
        event: &gerrit::CommentAddedEvent,
        is_human: bool,
    ) -> Result<Option<String>, String> {
        self.lua
            .context(|context| -> Result<Option<String>, String> {
                let globals = context.globals();

                let lua_format_comment_added: LuaFunction =
                    globals
                        .get(LUA_FORMAT_COMMENT_ADDED)
                        .map_err(|_| "format_approval function missing".to_string())?;
                let lua_event = to_lua_via_json(event, context)
                    .map_err(|e| format!("failed to serialize event: {}", e))?;
                let lua_result = lua_format_comment_added
                    .call::<_, LuaValue>((lua_event, is_human))
                    .map_err(|err| format!("lua formatting function failed: {}", err))?;

                match lua_result {
                    LuaValue::Nil => Ok(None),
                    _ => Ok(Some(String::from_lua(lua_result, context).map_err(
                        |e| format!("failed to convert formatting result to string: {}", e),
                    )?)),
                }
            })
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
        let res = Formatter::default().format_comment_added(&event, true);
        // Result<Option<String>, _> -> Result<Option<&str>, _>
        let res = res.as_ref().map(|o| o.as_ref().map(String::as_str));
        assert_eq!(
            res,
            Ok(Some("[Some review.](http://localhost/42) ([demo-project](http://localhost/q/project:demo-project+status:open)) 👍 +2 (Code-Review) from [Approver](http://localhost/q/reviewer:approver@approvers.com+status:open)\n\n> Just a buggy script. FAILURE<br>\n> And more problems. FAILURE"))
        );
    }

    #[test]
    fn format_approval_unknown_labels() {
        let mut event = get_event();
        event.approvals[0].approval_type = String::from("Some-New-Type");
        let res = Formatter::default().format_comment_added(&event, true);
        // Result<Option<String>, _> -> Result<Option<&str>, _>
        let res = res.as_ref().map(|o| o.as_ref().map(String::as_str));
        assert_eq!(
            res,
            Ok(Some("[Some review.](http://localhost/42) ([demo-project](http://localhost/q/project:demo-project+status:open)) 👍 +2 (Some-New-Type) from [Approver](http://localhost/q/reviewer:approver@approvers.com+status:open)\n\n> Just a buggy script. FAILURE<br>\n> And more problems. FAILURE"))
        );
    }

    #[test]
    fn format_approval_multiple_labels() {
        let mut event = get_event();
        event.approvals.push(gerrit::Approval {
            approval_type: "Verified".to_string(),
            description: "Verified".to_string(),
            value: "1".to_string(),
            old_value: None,
        });
        let res = Formatter::default().format_comment_added(&event, true);
        // Result<Option<String>, _> -> Result<Option<&str>, _>
        let res = res.as_ref().map(|o| o.as_ref().map(String::as_str));
        assert_eq!(
            res,
            Ok(Some("[Some review.](http://localhost/42) ([demo-project](http://localhost/q/project:demo-project+status:open)) 👍 +2 (Code-Review), ✔ +1 (Verified) from [Approver](http://localhost/q/reviewer:approver@approvers.com+status:open)\n\n> Just a buggy script. FAILURE<br>\n> And more problems. FAILURE"))
        );
    }

    #[test]
    fn format_approval_no_approvals() {
        let mut event = get_event();
        event.approvals.clear();
        let res = Formatter::default().format_comment_added(&event, true);
        // Result<Option<String>, _> -> Result<Option<&str>, _>
        let res = res.as_ref().map(|o| o.as_ref().map(String::as_str));
        assert_eq!(res, Ok(None));
    }

    #[test]
    fn test_format_comments() {
        let mut event = get_event();
        let (change, patchset) = get_change_with_comments();
        event.comment = "(1 comment)".to_string();
        event.change = change;
        event.patchset = patchset;

        let res = Formatter::default()
            .format_comment_added(&event, true)
            .expect("format failed")
            .expect("no comments");

        assert!(res.ends_with("`/COMMIT_MSG`\n\n> [Line 1](http://localhost:8080/#/c/1/1//COMMIT_MSG@1) by [jdoe](http://localhost:8080/q/reviewer:john.doe@localhost+status:open): This is a multiline\n> comment\n> on some change.\n"), "no inline comments: {:?}", res);
    }
}
