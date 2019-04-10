use itertools::Itertools as _;

use rlua::{Function as LuaFunction, Lua};

use gerritbot_gerrit as gerrit;

const UNKNOWN_USER: &str = "<unknown user>";

#[derive(Debug, Clone)]
pub struct Formatter {
    format_script: String,
}

impl Default for Formatter {
    fn default() -> Self {
        Self {
            format_script: get_default_format_script().to_string(),
        }
    }
}

fn get_default_format_script() -> &'static str {
    const DEFAULT_FORMAT_SCRIPT: &str = include_str!("../../scripts/format.lua");
    check_format_script_syntax(DEFAULT_FORMAT_SCRIPT)
        .unwrap_or_else(|err| panic!("invalid format script: {}", err));
    DEFAULT_FORMAT_SCRIPT
}

fn check_format_script_syntax(script_source: &str) -> Result<(), String> {
    let lua = Lua::new();
    lua.context(|context| {
        let globals = context.globals();
        context
            .load(script_source)
            .exec()
            .map_err(|err| format!("syntax error: {}", err))?;
        let _: LuaFunction = globals.get("main").map_err(|_| "main function missing")?;
        Ok(())
    })
}

impl Formatter {
    pub fn new(format_script: String) -> Result<Self, String> {
        check_format_script_syntax(&format_script)?;
        Ok(Self { format_script })
    }

    pub fn format_approval(
        &self,
        event: &gerrit::CommentAddedEvent,
        approval: &gerrit::Approval,
        is_human: bool,
    ) -> Result<String, String> {
        fn create_lua_event<'lua>(
            context: rlua::Context<'lua>,
            event: &gerrit::CommentAddedEvent,
            approval: &gerrit::Approval,
            is_human: bool,
        ) -> Result<rlua::Table<'lua>, rlua::Error> {
            let lua_event = context.create_table()?;
            lua_event.set(
                "approver",
                event
                    .author
                    .username
                    .as_ref()
                    .cloned()
                    .unwrap_or_else(|| UNKNOWN_USER.to_string()),
            )?;
            lua_event.set("comment", event.comment.clone())?;
            lua_event.set("value", approval.value.parse().unwrap_or(0))?;
            lua_event.set("type", approval.approval_type.clone())?;
            lua_event.set("url", event.change.url.clone())?;
            lua_event.set("subject", event.change.subject.clone())?;
            lua_event.set("project", event.change.project.clone())?;
            lua_event.set("is_human", is_human)?;
            Ok(lua_event)
        }

        let lua = Lua::new();
        lua.context(|context| -> Result<String, String> {
            let globals = context.globals();
            context
                .load(&self.format_script)
                .exec()
                .map_err(|err| format!("syntax error: {}", err))?;
            let f: LuaFunction = globals
                .get("main")
                .map_err(|_| "main function missing".to_string())?;
            let lua_event = create_lua_event(context, event, approval, is_human)
                .map_err(|err| format!("failed to create lua event table: {}", err))?;

            f.call::<_, String>(lua_event)
                .map_err(|err| format!("lua formatting function failed: {}", err))
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

    fn format_inline_comments(
        &self,
        change: &gerrit::Change,
        patchset: gerrit::Patchset,
    ) -> Option<String> {
        let change_number = change.number;
        let base_url = {
            let last_slash = change.url.rfind('/').unwrap();
            &change.url[..last_slash]
        };
        let patch_set_number = patchset.number;

        patchset.comments.map(|mut comments| {
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
    }

    pub fn format_message_with_comments(
        &self,
        message: String,
        change: &gerrit::Change,
        patchset: gerrit::Patchset,
    ) -> String {
        if let Some(additional_message) = self.format_inline_comments(change, patchset) {
            format!("{}\n\n{}", message, additional_message)
        } else {
            message
        }
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
        let res = Formatter::default().format_approval(
            &event,
            &event.approvals[0],
            true,
        );
        assert_eq!(
            res,
            Ok("[Some review.](http://localhost/42) (demo-project) 👍 +2 (Code-Review) from approver\n\n> Just a buggy script. FAILURE<br>\n> And more problems. FAILURE".to_string())
        );
    }

    #[test]
    fn format_approval_filters_specific_messages() {
        let mut event = get_event();
        event.approvals[0].approval_type = String::from("Some new type");
        let res = Formatter::default().format_approval(
            &event,
            &event.approvals[0],
            true,
        );
        assert_eq!(res.map(|s| s.is_empty()), Ok(true));
    }

    #[test]
    fn test_format_comments() {
        let (change, mut patchset) = get_change_with_comments();
        patchset.comments = None;
        assert_eq!(
            Formatter::default().format_inline_comments(&change, patchset),
            None
        );

        let (change, patchset) = get_change_with_comments();
        assert_eq!(Formatter::default().format_inline_comments(&change, patchset),
                   Some("`/COMMIT_MSG`\n\n> [Line 1](http://localhost:8080/#/c/1/1//COMMIT_MSG@1) by jdoe: This is a multiline\n> comment\n> on some change.".into()));
    }
}
