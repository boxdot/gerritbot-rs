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

local APPROVAL_ICONS = {
    ["WaitForVerification"] = {[-1] = "⏳"},
    ["Code-Review"] = {[-2] = "👎", [-1] = "🤷", [1] = "👌", [2] = "👍"},
    ["Verified"] = {[-1] = "❌", [1] = "✔"},
    -- fallback
    ["*"] = {[-2] = "👎", [-1] = "🙅", [1] = "🙆", [2] = "👍"},
}

local function get_approval_icon(type, value, old_value)
    if value == 0 then
        if old_value ~= 0 then
            return "📝"
        else
            return nil
        end
    end

    type_icons = APPROVAL_ICONS[type] or APPROVAL_ICONS["*"]

    return type_icons[value]
end

local function format_approval(approval)
    local approval_value = tonumber(approval.value)
    local old_approval_value = tonumber(approval.old_value or "0")
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
        -- XXX: change <br> to \n
        return "\n\n" .. table.concat(lines, "<br>\n")
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
                    -- TODO: use format_user
                    comment.reviewer.username,
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

-- Filter and format messages
-- return nil to filter the message
function format_comment_added(event, is_human)
    local change = event.change
    local patchset = event.patchSet
    local base_url = get_gerrit_base_url(change.url)

    local msg = format_change_subject(change) .. " (" .. format_change_project(base_url, change) .. ")"

    local formatted_approvals = {}

    for _i, approval in ipairs(event.approvals) do
        local formatted_approval = format_approval(approval)

        if formatted_approval then
            table.insert(formatted_approvals, formatted_approval)
        end
    end

    if #formatted_approvals > 0 then
        msg = msg .. " " .. table.concat(formatted_approvals, ", ")
    elseif not (is_human and patchset.comments and #patchset.comments > 0) then
        -- TODO: messages without approvals should still be formatted since they
        -- can be comment responses. This should be handled at a higher level.
        -- Keep this here for now to prevent spamming.
        return
    end

    msg = msg .. " from " .. format_user(base_url, event.author, "reviewer")
    msg = msg .. (format_comment(event.comment, is_human) or "")
    msg = msg .. (format_inline_comments(base_url, change, patchset) or "")

    return msg
end
