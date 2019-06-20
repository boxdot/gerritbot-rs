use rlua::{prelude::*, StdLib as LuaStdLib};
use serde::Serialize;

use gerritbot_gerrit as gerrit;

use crate::state::{User, NOTIFICATION_FLAGS};
use crate::version::VersionInfo;
use crate::IsHuman;

pub const DEFAULT_FORMAT_SCRIPT: &str = include_str!("format.lua");

pub trait MessageInput: Serialize {
    const FORMAT_FUNCTION: &'static str;
}

impl<'a> MessageInput for &'a gerrit::CommentAddedEvent {
    const FORMAT_FUNCTION: &'static str = "format_comment_added";
}

impl<'a> MessageInput for &'a gerrit::ReviewerAddedEvent {
    const FORMAT_FUNCTION: &'static str = "format_reviewer_added";
}

impl<'a> MessageInput for &'a gerrit::ChangeMergedEvent {
    const FORMAT_FUNCTION: &'static str = "format_change_merged";
}

impl<'a> MessageInput for &'a gerrit::ChangeAbandonedEvent {
    const FORMAT_FUNCTION: &'static str = "format_change_abandoned";
}

impl<'a> MessageInput for &'a VersionInfo {
    const FORMAT_FUNCTION: &'static str = "format_version_info";
}

#[derive(Serialize)]
pub struct HelpMessage;

impl<'a> MessageInput for HelpMessage {
    const FORMAT_FUNCTION: &'static str = "format_help";
}

#[derive(Serialize)]
pub struct GreetingMessage;

impl<'a> MessageInput for GreetingMessage {
    const FORMAT_FUNCTION: &'static str = "format_greeting";
}

#[derive(Serialize)]
struct StatusDetails {
    user_enabled: bool,
    enabled_user_count: usize,
}

impl MessageInput for StatusDetails {
    const FORMAT_FUNCTION: &'static str = "format_status";
}

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

        let is_human = context
            .create_function(|_, user| {
                let user: gerrit::User = rlua_serde::from_value(user)?;
                Ok(user.is_human())
            })
            .map_err(|e| format!("failed to create is_human function: {}", e))?;

        globals
            .set("is_human", is_human)
            .map_err(|e| format!("failed to set is_human function: {}", e))?;

        context
            .load(script_source)
            .set_name("format.lua")
            .map_err(|e| format!("failed to set chunk name: {}", e))?
            .exec()
            .map_err(|err| format!("syntax error: {}", err))?;

        Ok(())
    })?;
    Ok(lua)
}

fn get_flags_table<'lua>(user: &User, lua: rlua::Context<'lua>) -> rlua::Result<rlua::Table<'lua>> {
    lua.create_table_from(NOTIFICATION_FLAGS.iter().cloned().filter_map(|flag| {
        if user.has_flag(flag) {
            Some((flag.to_string(), true))
        } else {
            None
        }
    }))
}

impl Formatter {
    pub fn new(format_script: &str) -> Result<Self, String> {
        Ok(Self {
            lua: load_format_script(&format_script)?,
        })
    }

    fn format_lua<'lua, I>(
        lua: rlua::Context<'lua>,
        user: Option<&User>,
        input: I,
    ) -> Result<Option<String>, String>
    where
        I: MessageInput,
    {
        let globals = lua.globals();
        let function_name = I::FORMAT_FUNCTION;

        let format_function: LuaFunction = globals
            .get(function_name)
            .map_err(|_| format!("{} function missing", function_name))?;

        let format_args = (
            rlua_serde::to_value(lua, input)
                .map_err(|e| format!("failed to serialize event: {}", e))?,
            if let Some(user) = user {
                get_flags_table(user, lua)
                    .map(LuaValue::Table)
                    .map_err(|err| format!("failed to create flags table: {}", err))?
            } else {
                LuaNil
            },
        );

        let result = format_function
            .call::<_, LuaValue>(format_args)
            .map_err(|err| format!("lua formatting function failed: {}", err))?;

        FromLua::from_lua(result, lua)
            .map_err(|e| format!("failed to convert formatting result: {}", e))
    }

    pub fn format_message<I: MessageInput>(
        &self,
        user: Option<&User>,
        input: I,
    ) -> Result<Option<String>, String> {
        self.lua
            .context(move |lua| Formatter::format_lua(lua, user, input))
    }

    pub fn format_status(
        &self,
        user: Option<&User>,
        enabled_user_count: usize,
    ) -> Result<Option<String>, String> {
        self.format_message(
            user,
            StatusDetails {
                user_enabled: user
                    .map(|u| u.has_any_flag(NOTIFICATION_FLAGS))
                    .unwrap_or(false),
                enabled_user_count,
            },
        )
    }

    pub fn format_greeting(&self) -> Result<Option<String>, String> {
        self.format_message(None, GreetingMessage)
    }

    pub fn format_help(&self) -> Result<Option<String>, String> {
        self.format_message(None, HelpMessage)
    }
}

#[cfg(test)]
mod test {
    use lazy_static::lazy_static;

    use gerritbot_spark as spark;

    use crate::state::State;

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

    lazy_static! {
        static ref FORMAT_TEST_STATE: State = {
            let mut state = State::new();
            state.add_user(spark::EmailRef::new("some@example.com"));
            state
        };
        static ref FORMAT_TEST_USER: &'static User = FORMAT_TEST_STATE
            .find_user(spark::EmailRef::new("some@example.com"))
            .unwrap();
    }

    #[test]
    fn test_format_approval() {
        let event = get_event();
        let res = Formatter::default().format_message(Some(&FORMAT_TEST_USER), &event);
        // Result<Option<String>, _> -> Result<Option<&str>, _>
        let res = res.as_ref().map(|o| o.as_ref().map(String::as_str));
        assert_eq!(
            res,
            Ok(Some("[Some review.](http://localhost/42) ([demo-project](http://localhost/q/project:demo-project+status:open)) 👍 +2 (Code-Review) from [Approver](http://localhost/q/reviewer:approver@approvers.com+status:open)\n\n> Just a buggy script. FAILURE\n\n> And more problems. FAILURE"))
        );
    }

    #[test]
    fn format_approval_unknown_labels() {
        let mut event = get_event();
        event
            .approvals
            .as_mut()
            .map(|approvals| approvals[0].approval_type = String::from("Some-New-Type"));
        let res = Formatter::default().format_message(Some(&FORMAT_TEST_USER), &event);
        // Result<Option<String>, _> -> Result<Option<&str>, _>
        let res = res.as_ref().map(|o| o.as_ref().map(String::as_str));
        assert_eq!(
            res,
            Ok(Some("[Some review.](http://localhost/42) ([demo-project](http://localhost/q/project:demo-project+status:open)) 🤩 +2 (Some-New-Type) from [Approver](http://localhost/q/reviewer:approver@approvers.com+status:open)\n\n> Just a buggy script. FAILURE\n\n> And more problems. FAILURE"))
        );
    }

    #[test]
    fn format_approval_multiple_labels() {
        let mut event = get_event();
        event.approvals.as_mut().map(|approvals| {
            approvals.push(gerrit::Approval {
                approval_type: "Verified".to_string(),
                description: Some("Verified".to_string()),
                value: "1".to_string(),
                old_value: None,
                by: None,
            })
        });
        let res = Formatter::default().format_message(Some(&FORMAT_TEST_USER), &event);
        // Result<Option<String>, _> -> Result<Option<&str>, _>
        let res = res.as_ref().map(|o| o.as_ref().map(String::as_str));
        assert_eq!(
            res,
            Ok(Some("[Some review.](http://localhost/42) ([demo-project](http://localhost/q/project:demo-project+status:open)) 👍 +2 (Code-Review), 🌞 +1 (Verified) from [Approver](http://localhost/q/reviewer:approver@approvers.com+status:open)\n\n> Just a buggy script. FAILURE\n\n> And more problems. FAILURE"))
        );
    }

    #[test]
    fn format_approval_no_approvals() {
        let mut event = get_event();
        event.approvals = None;
        let res = Formatter::default().format_message(Some(&FORMAT_TEST_USER), &event);
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
            .format_message(Some(&FORMAT_TEST_USER), &event)
            .expect("format failed")
            .expect("no comments");

        assert!(res.ends_with("`/COMMIT_MSG`\n\n> [Line 1](http://localhost:8080/#/c/1/1//COMMIT_MSG@1) by [jdoe](http://localhost:8080/q/reviewer:john.doe@localhost+status:open): This is a multiline\n> comment\n> on some change.\n"), "no inline comments: {:?}", res);
    }
}
