-- Filter and format messages
-- return empty string to filter the message
function main(event)
    if event.type ~= "Code-Review" and event.type ~= "WaitForVerification" and event.type ~= "Verified" then
        return ""
    end

    if string.match(event.type, "WaitForVerification") then
        icon = "⌛"
    elseif event.value > 0 then
        icon = "👍"
    elseif event.value == 0 then
        icon = "📝"
    else
        icon = "👎"
    end

    sign = ""
    if event.value > 0 then
        sign = "+"
    end

    -- TODO: when Spark will allow to format text with different colors, set
    -- green resp. red color here.
    f = "[%s](%s) (%s) %s %s%s (%s) from %s"
    msg = string.format(f, event.subject, event.url, event.project, icon, sign, event.value, event.type, event.approver)

    len = 0
    lines = {}
    for line in string.gmatch(event.comment, "[^\r\n]+") do
        if event.is_human and not line:match "^Patch Set" then
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
