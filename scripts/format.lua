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

-- Filter and format messages
-- return nil to filter the message
function format_comment_added(event, is_human)
    local change = event.change
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
    else
        -- TODO: messages without approvals should still be formatted since they
        -- can be comment responses. This should be handled at a higher level.
        -- Keep this here for now to prevent spamming.
        return
    end

    msg = msg .. " from " .. format_user(base_url, event.author, "reviewer")

    local len = 0
    local lines = {}
    for line in string.gmatch(event.comment, "[^\r\n]+") do
        if is_human and not line:match "^Patch Set" and not line:match "%(%d+ comments?%)" then
            table.insert(lines, "> " .. line)
            len = len + 1
        elseif string.match(line, "FAILURE") then
            table.insert(lines, "> " .. line)
            len = len + 1
        end
    end

    if len == 0 then
        return msg
    else
        lines = table.concat(lines, "<br>\n")
        return msg .. "\n\n" .. lines
    end
end
