
#[derive(Deserialize, Debug)]
pub struct User {
    name: String,
    username: String,
    email: Option<String>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Approval {
    #[serde(rename="type")]
    approval_type: String,
    description: String,
    value: String,
    old_value: String,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PatchSet {
    number: String,
    revision: String,
    parents: Vec<String>,
    #[serde(rename="ref")]
    reference: String,
    uploader: User,
    created_on: u32,
    author: User,
    is_draft: bool,
    kind: String,
    size_insertions: u32,
    size_deletions: u32,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    project: String,
    branch: String,
    id: String,
    number: String,
    subject: String,
    owner: User,
    url: String,
    commit_message: String,
    status: String,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ChangeKey {
    id: String,
}

// Only specific event are accepted by this type by design!
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    author: User,
    approvals: Vec<Approval>,
    comment: Option<String>,
    #[serde(rename="patchSet")]
    patchset: PatchSet,
    change: Change,
    project: String,
    #[serde(rename="refName")]
    ref_name: String,
    #[serde(rename="changeKey")]
    changekey: ChangeKey,
    #[serde(rename="type")]
    event_type: String,
    #[serde(rename="eventCreatedOn")]
    created_on: u32,
}
