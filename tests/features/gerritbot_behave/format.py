class URLs:
    def __init__(self, context):
        self.projects = ProjectURLs(context)
        self.users = UserURLs(context)
        self.changes = ChangeURLs(context)


class URLsBase:
    def __init__(self, context):
        self.context = context


class ProjectURLs(URLsBase):
    def __getitem__(self, project):
        return f"{self.context.gerrit_http_url}/q/project:{project}+status:open"


class UserURLs(URLsBase):
    def __getitem__(self, username):
        return UserURL(self.context, username)


class UserURL:
    def __init__(self, context, username, role="owner"):
        self.context = context
        self.username = username
        self.role = role

    def __str__(self):
        email = self.context.persons.get(self.username).email
        return f"{self.context.gerrit_http_url}/q/{self.role}:{email}+status:open"

    def __getattr__(self, role):
        return type(self)(self.context, self.username, role)


class ChangeURLs(URLsBase):
    def __getitem__(self, change_id):
        if change_id == "last":
            change_id = self.context.last_created_change["_number"]

        return f"{self.context.gerrit_http_url}/{change_id}"
