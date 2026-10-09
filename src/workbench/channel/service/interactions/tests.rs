use super::*;
use crate::workbench::channel::interaction::{Decode, decode_request};
use serde_json::json;

#[test]
fn question_buttons_are_bounded_opaque_and_advance_without_partial_answers() {
    let value = json!({"id":"synthetic-rpc","method":"item/tool/requestUserInput","params":{
    "threadId":Uuid::from_u128(2),"turnId":"synthetic-turn","itemId":"synthetic-item",
    "questions":[
        {"id":"a","header":"First","question":"Choose first","isOther":false,"isSecret":false,
         "options":[{"label":"a1","description":"one"},{"label":"a2","description":"two"},
            {"label":"a3","description":"three"},{"label":"a4","description":"four"}]},
        {"id":"b","header":"Second","question":"Choose second","isOther":false,"isSecret":false,
         "options":[{"label":"b1","description":"one"},{"label":"b2","description":"two"}]}
    ]}});
    let Decode::Supported(request) = decode_request(&value, None).unwrap() else {
        panic!("synthetic options");
    };
    let (first, refs) = buttons(&request, &[]);
    assert_eq!(first.len(), 4);
    assert_eq!(first[0].label, "1.1");
    assert!(
        refs.values()
            .all(|a| matches!(a, Action::Choose { question: 0, .. }))
    );
    assert!(
        first
            .iter()
            .all(|b| b.reference.len() == 32 && b.reference.bytes().all(|c| c.is_ascii_hexdigit()))
    );
    let selected = vec![request.select(0, 2).unwrap()];
    let (second, second_refs) = buttons(&request, &selected);
    assert_eq!(second.len(), 2);
    assert_eq!(second[0].label, "2.1");
    assert!(second_refs.keys().all(|r| !refs.contains_key(r)));
    assert!(
        request
            .response(
                &Action::Choose {
                    question: 0,
                    option: 2
                },
                &[]
            )
            .unwrap()
            .is_none()
    );
    assert!(
        request
            .response(
                &Action::Choose {
                    question: 1,
                    option: 1
                },
                &selected
            )
            .unwrap()
            .is_some()
    );
}
