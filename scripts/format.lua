-- Filter and format messages
-- return nil to filter the message
function format_approval(event, approval, is_human)
    if approval.type ~= "Code-Review" and approval.type ~= "WaitForVerification" and approval.type ~= "Verified" then
        return
    end

    approval_value = tonumber(approval.value)

    if string.match(approval.type, "WaitForVerification") then
        icon = "⌛"
    elseif approval_value > 0 then
        icon = "👍"
    elseif approval_value == 0 then
        icon = "📝"
    else
        icon = "👎"
    end

    sign = ""
    if approval_value > 0 then
        sign = "+"
    end

    -- TODO: when Spark will allow to format text with different colors, set
    -- green resp. red color here.
    f = "[%s](%s) (%s) %s %s%s (%s) from %s"
    msg = string.format(f, event.change.subject, event.change.url, event.change.project, icon, sign, approval_value, approval.type, event.author.username)

    len = 0
    lines = {}
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
