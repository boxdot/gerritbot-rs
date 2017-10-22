function main(approver, comment, value, type, url, subject)
    if string.match(type, "WaitForVerification") then
        icon = "⌛"
    elseif value > 0 then
        icon = "👍"
    elseif value == 0 then
        icon = "👉"
    else
        icon = "👎"
    end

    sign = ""
    if value > 0 then
        sign = "+"
    end

    -- TODO: when Spark will allow to format text with different colors, set
    -- green resp. red color here.
    f = "[%s](%s) %s %s%s (%s) from %s"
    msg = string.format(f, subject, url, icon, sign, value, type, approver)

    len = 0
    lines = {}
    for line in string.gmatch(comment, "[^\r\n]+") do
        if string.match(line, "FAILURE") then
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
