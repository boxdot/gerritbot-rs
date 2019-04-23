import itertools

from hamcrest import *


@when("we check for messages by the bot")
def step_impl(context):
    context.bot.get_messages()


@then("there are no messages")
def step_impl(context):
    assert_that(context.bot.current_messages, empty())


@then('there is a message for {person} which includes the text "{text}"')
def step_impl(context, person, text):
    context.execute_steps(
        f'''
    then there is a message for {person} which includes the following text:
    """
    {text}
    """
    '''
    )


@then("there is a message for {person} which includes the following text")
def step_impl(context, person):
    text = context.text
    person = context.persons.get(person)
    messages_for_person = context.bot.get_messages_for_person(person)
    item_matcher = has_entry("text", contains_string(text))
    assert_that(messages_for_person, has_item(item_matcher))
    context.last_matched_message = next(
        (m for m in messages_for_person if item_matcher.matches(m))
    )


@then("there is a message for {person} with the following text")
def step_impl(context, person):
    text = context.text.format(context=context)
    person = context.persons.get(person)
    messages_for_person = context.bot.get_messages_for_person(person)
    item_matcher = has_entry("text", equal_to(text))
    assert_that(messages_for_person, has_item(item_matcher))
    context.last_matched_message = next(
        (m for m in messages_for_person if item_matcher.matches(m))
    )


@then('there is no message for {person} which includes the text "{text}"')
def step_impl(context, person, text):
    context.execute_steps(
        f'''
    then there is no message for {person} which includes the following text:
    """
    {text}
    """
    '''
    )


@then("there is no message for {person} which includes the following text")
def step_impl(context, person):
    text = context.text
    person = context.persons.get(person)
    messages_for_person = context.bot.get_messages_for_person(person)
    item_matcher = has_entry("text", contains_string(text))
    assert_that(messages_for_person, is_not(has_item(item_matcher)))


@then('this message includes the text "{text}"')
def step_impl(context, text):
    assert_that(context.last_matched_message, has_entry("text", contains_string(text)))


@then('this message does not include the text "{text}"')
def step_impl(context, text):
    assert_that(
        context.last_matched_message, has_entry("text", is_not(contains_string(text)))
    )


@step("{sender} sends the {command} command to the bot")
def step_impl(context, sender, command):
    if sender == "everybody":
        for person in context.persons:
            context.bot.send_message(person, command)
    else:
        sender = context.persons.get(sender)
        context.bot.send_message(sender, command)
