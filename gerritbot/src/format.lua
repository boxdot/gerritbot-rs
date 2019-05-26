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

-- Format submittable message
local function format_submittable(submit_records)
    for _i, submit_record in ipairs(submit_records or {}) do
        if submit_record.status == "OK" then
            return ", 🏁 Submittable"
        end
    end
end

-- Filter and format messages
-- return nil to filter the message
function format_comment_added(event, flags, is_human)
    local change = event.change

    if not is_human and change.status ~= "NEW" then
        return
    end

    local patchset = event.patchSet
    local base_url = get_gerrit_base_url(change.url)
    local formatted_approvals = flags["notify_review_approvals"] and format_approvals(event.approvals)
    local formatted_submittable_message = flags["notify_review_approvals"] and format_submittable(change.submitRecords)
    local formatted_inline_comments = flags["notify_review_inline_comments"] and format_inline_comments(base_url, change, patchset)
    local formatted_comment = (
        flags["notify_review_comments"]
        or formatted_approvals
        or (is_human and formatted_inline_comments)
    ) and format_comment(event.comment, is_human)

    if formatted_approvals
        or formatted_comment
        or formatted_inline_comments
        or formatted_submittable_message
    then
        local msg = format_change_subject(change) .. " (" .. format_change_project(base_url, change) .. ")"
        msg = msg .. (formatted_approvals or " comments")
        msg = msg .. " from " .. format_user(base_url, event.author, "reviewer")
        msg = msg .. (formatted_submittable_message or "")
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
