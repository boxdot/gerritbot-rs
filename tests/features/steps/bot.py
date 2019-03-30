from hamcrest import *


@when(u"we check for messages by the bot")
def step_impl(context):
    context.bot.get_messages()


@then(u"there are no messages")
def step_impl(context):
    assert_that(context.bot.current_messages, empty())


@then(u'there is a message for {person} which includes the text "{text}"')
def step_impl(context, person, text):
    person = context.persons.get(person)
    messages_for_person = context.bot.get_messages_for_person(person)
    item_matcher = has_entry("text", contains_string(text))
    assert_that(messages_for_person, has_item(item_matcher))
    context.last_matched_message = next(
        (m for m in messages_for_person if item_matcher.matches(m))
    )


@then(u'this message includes the text "{text}"')
def step_impl(context, text):
    assert_that(context.last_matched_message, has_entry("text", contains_string(text)))


@given(u"{sender} sends the {command} command to the bot")
def step_impl(context, sender, command):
    sender = context.persons.get(sender)
    context.bot.send_message(sender, command)
