-- Get the Gerrit base URL from the given change URL.
local function get_gerrit_base_url(change_url)
    return string.sub(change_url, 1, #change_url - string.find(string.reverse(change_url), "/"))
end

-- Get a URL for a Gerrit query.
local function get_query_url(base_url, query, ...)
    return string.format("%s/q/%s", base_url, string.format(query, ...))
end

-- Format a link.
local function format_link(text, target)
    return string.format("[%s](%s)", text, target)
end

-- Format a link to a Gerrit query.
local function format_query_link(base_url, text, query, ...)
    return format_link(text, get_query_url(base_url, query, ...))
end

-- Format a link to a user.
local function format_user(base_url, user, role)
    return format_query_link(
        base_url,
        user.name or user.email,
        "%s:%s+status:open",
        role, user.email
    )
end

-- Format a change's subject.
local function format_change_subject(change)
    return format_link(change.subject, change.url)
end

-- Format a change's project.
local function format_change_project(base_url, change)
    local result = format_query_link(
        base_url,
        change.project,
        "project:%s+status:open",
        change.project
    )

    if change.branch ~= "master" then
        result = result .. ", branch:" .. change.branch
    end

    if change.topic then
        result = result .. ", topic:" .. format_query_link(
            base_url,
            change.topic,
            "topic:%s+status:open",
            change.topic
        )
    end

    return result
end

-- Lua string pattern → table of emoji
local APPROVAL_ICONS = {
    {"WaitForVerification", {[-1] = "⏳"}},
    {"Code[-]Review", {[-2] = "👎", [-1] = "✋", [1] = "👌", [2] = "👍"}},
    {"Verified", {[-1] = "⛈️", [1] = "🌞"}},
    {"QA", {[-1] = "❌", [1] = "✅"}},
    -- fallback
    {"", {[-2] = "😬", [-1] = "🤨", [1] = "😉", [2] = "🤩"}},
}

local function get_approval_icon(type, value, old_value)
    if value == 0 then
        if old_value ~= 0 then
            return "📝"
        else
            return nil
        end
    end

    for _, item in pairs(APPROVAL_ICONS) do
        local type_pattern = item[1]
        local type_icons = item[2]

        if string.match(string.lower(type), string.lower(type_pattern)) then
            return type_icons[value]
        end
    end
end

local function format_approval(approval)
    local approval_value = tonumber(approval.value) or 0
    local old_approval_value = tonumber(approval.oldValue) or 0

    if old_approval_value == approval_value then
        return nil
    end

    local icon = get_approval_icon(approval.type, approval_value, old_approval_value)

    local sign = ""
    if approval_value > 0 then
        sign = "+"
    end

    if icon then
        icon = icon .. " "
    else
        icon = ""
    end

    return string.format("%s%s%s (%s)", icon, sign, approval_value, approval.type)
end

-- return an iterator over the lines in the given string
local function lines_iter(s)
    return string.gmatch(s, "[^\r\n]+")
end

local function format_comment(comment, is_human)
    local lines = {}

    for line in lines_iter(comment) do
        if is_human and not line:match "^Patch Set" and not line:match "%(%d+ comments?%)" then
            table.insert(lines, "> " .. line)
        elseif string.match(line, "FAILURE") then
            table.insert(lines, "> " .. line)
        end
    end

    if #lines > 0 then
        return "\n\n" .. table.concat(lines, "\n\n")
    end
end

local function format_inline_comment(base_url, change, patchset, comment)
    local lines = {}

    for line in lines_iter(comment.message) do
        if #lines == 0 then
            local url = string.format(
                "%s/#/c/%s/%s/%s@%s",
                base_url,
                change.number,
                patchset.number,
                comment.file,
                comment.line
            )

            table.insert(
                lines,
                string.format(
                    "> [Line %s](%s) by %s: %s",
                    comment.line,
                    url,
                    format_user(base_url, comment.reviewer, "reviewer"),
                    line
                )
            )

        else
            table.insert(lines, "> " .. line)
        end
    end

    return table.concat(lines, "\n")
end

local function format_inline_comments(base_url, change, patchset)
    local lines = {}
    local comments = patchset.comments or {}

    table.sort(comments, function (c1, c2) return c1.file < c2.file end)

    local file

    for _i, comment in ipairs(comments) do
        if comment.file ~= file then
            file = comment.file
            table.insert(lines, string.format("`%s`", file))
        end

        table.insert(lines, format_inline_comment(base_url, change, patchset, comment))
    end

    if #lines > 0 then
        return "\n\n" .. table.concat(lines, "\n\n") .. "\n"
    end
end

-- Format approvals.
-- Note: sorts given approval list.
local function format_approvals(approvals)
    local formatted_approvals = {}

      table.sort(approvals, function(a1, a2) return a1.type < a2.type end)

      for _i, approval in ipairs(approvals) do
          local formatted_approval = format_approval(approval)

          if formatted_approval then
              table.insert(formatted_approvals, formatted_approval)
          end
      end

    if #formatted_approvals > 0 then
        return " " .. table.concat(formatted_approvals, ", ")
    end
end

-- Format change status
local function format_change_status(change)
    if change.status == "NEW" then
        for _i, submit_record in ipairs(change.submitRecords or {}) do
            if submit_record.status == "OK" then
                return ", 🏁 Submittable"
            end
        end
    elseif change.status == "MERGED" then
        return ", 📦 Merged"
    elseif change.status == "ABANDONED" then
        return ", ☠  Abandoned"
    end
end

-- Filter and format messages
-- return nil to filter the message
function format_comment_added(event, flags)
    local is_human = is_human(event.author)
    local change = event.change

    if not is_human and change.status ~= "NEW" then
        return
    end

    local patchset = event.patchSet
    local base_url = get_gerrit_base_url(change.url)
    local formatted_approvals = flags["notify_review_approvals"] and format_approvals(event.approvals or {})
    local formatted_status_message = flags["notify_review_approvals"] and format_change_status(change)
    local formatted_inline_comments = flags["notify_review_inline_comments"] and format_inline_comments(base_url, change, patchset)
    local formatted_comment = (
        flags["notify_review_comments"]
        or formatted_approvals
        or (is_human and formatted_inline_comments)
        or (event.author.email == change.owner.email and flags["notify_review_responses"])
    ) and format_comment(event.comment, is_human)

    if formatted_approvals
        or formatted_comment
        or formatted_inline_comments
        or formatted_status_message
    then
        local msg = format_change_subject(change) .. " (" .. format_change_project(base_url, change) .. ")"
        msg = msg .. (formatted_approvals or " comments")
        msg = msg .. " from " .. format_user(base_url, event.author, "reviewer")
        msg = msg .. (formatted_status_message or "")
        msg = msg .. (formatted_comment or "")
        msg = msg .. (formatted_inline_comments or "")
        return msg
    end
end

function format_reviewer_added(event, flags)
    local change = event.change
    local base_url = get_gerrit_base_url(change.url)

    return string.format(
        "%s (%s) by %s 👓 Added as reviewer",
        format_change_subject(change),
        format_change_project(base_url, change),
        format_user(base_url, change.owner, "owner")
    )
end

function format_change_merged(event, flags)
    local change = event.change
    local base_url = get_gerrit_base_url(change.url)

    return string.format(
        "%s (%s) 📦 Submitted by %s",
        format_change_subject(change),
        format_change_project(base_url, change),
        format_user(base_url, event.submitter, "owner")
    )
end

function format_change_abandoned(event, flags)
    local change = event.change
    local base_url = get_gerrit_base_url(change.url)

    return string.format(
        "%s (%s) ☠  Abandoned by %s",
        format_change_subject(change),
        format_change_project(base_url, change),
        format_user(base_url, event.abandoner, "owner")
    )
end

function format_version_info(version_info)
    return string.format(
        "%s %s (commit id: %s, built with Rust %s for %s on %s)",
        version_info.package_name,
        version_info.package_version,
        version_info.ci_commit_id or version_info.git_commit_id,
        version_info.rustc_version,
        version_info.target_triple,
        version_info.build_date
    )
end

function format_greeting()
    return [=[
Hi. I am GerritBot. I can watch Gerrit reviews for you, and notify you about new +1/-1's.

To enable notifications, just type in **enable**. A small note: your email in Spark and in Gerrit has to be the same. Otherwise, I can't match your accounts.

For more information, type in **help**.
]=]
end

local FLAG_DESCRIPTIONS = {
    notify_review_approvals = "Toggle notification messages for reviews with approvals (Code-Review, Verified etc.).",
    notify_review_comments = "Toggle notification messages for comments without approvals.",
    notify_review_inline_comments = "Toggle notifications messages for inline comments.",
    notify_review_responses = "Toggle notification on follow up comments to earlier review comments.",
    notify_reviewer_added = "Toggle notification messages when added as reviewer.",
    notify_change_abandoned = "Toggle notification when a change is abandoned.",
    notify_change_merged = "Toggle notification when a change is merged.",
}

local FLAG_SINGLE_LINE_FORMAT = "* `%s` -- %s"

function format_help()
    local flags = {}

    for flag_name, flag_description in pairs(FLAG_DESCRIPTIONS) do
        table.insert(flags, string.format(FLAG_SINGLE_LINE_FORMAT, flag_name, flag_description))
    end

    table.sort(flags)

    return [=[
Commands:

`enable` -- I will start notifying you.

`disable` -- I will stop notifying you.

`enable <flag>`, `disable <flag>` -- Enable or disable specific behavior.  The following flags are available:

]=] .. table.concat(flags, "\n") .. [=[


`filter <regex>` -- Filter all messages by applying the specified regex pattern. If the pattern matches, the message is filtered. The pattern is applied to the full text I send to you. Be aware, to send this command **not** in markdown mode, otherwise, Spark would eat some special characters in the pattern. For regex specification, cf. https://docs.rs/regex/0.2.10/regex/#syntax.

`filter enable` -- Enable the filtering of messages with the configured filter.

`filter disable` -- Disable the filtering of messages with the configured filter.

`status` -- Show if I am notifying you, and a little bit more information. 😉

`help` -- This message

This project is open source, feel free to help us at: https://github.com/boxdot/gerritbot-rs
]=]
end

function format_status(status_details, user_flags)
    local enabled = status_details.user_enabled
    local other_count = status_details.enabled_user_count - (enabled and 1 or 0)

    if not enabled and other_count == 0 then
        other_users_string = "no users"
    elseif enabled and other_count == 0 then
        other_users_string = "no other users"
    elseif not enabled and other_count == 1 then
        other_users_string = "one user"
    elseif enabled and other_count == 1 then
        other_users_string = "another user"
    elseif enabled then
        other_users_string = string.format("another %s users", other_count)
    else
        other_users_string = string.format("%s users", other_count)
    end

    local flag_strings = {}
    local flags_string

    for flag_name, flag_value in pairs(user_flags or {}) do
        if flag_value then
            local flag_description = FLAG_DESCRIPTIONS[flag_name]
            table.insert(flag_strings, string.format(FLAG_SINGLE_LINE_FORMAT, flag_name, flag_description))
        end
    end

    table.sort(flag_strings)

    if #flag_strings > 0 then
        flags_string = "The following flags are **enabled** for you: \n" .. table.concat(flag_strings, "\n") .. "."
    else
        flags_string = "No flags are enabled for you."
    end

    return string.format(
        "Notifications for you are **%s**. I am notifying %s.\n\n%s",
        status_details.user_enabled and "enabled" or "disabled",
        other_users_string,
        flags_string
    )
end
