use std::borrow::Cow;

use scoped_join_set::ScopedJoinSet;

#[tokio::test]
async fn borrowed_struct_result() {
    #[derive(Debug, PartialEq)]
    struct Borrowed<'a> {
        val: &'a i32,
    }

    let num = 123;
    let mut set = ScopedJoinSet::new();

    set.spawn(async { Borrowed { val: &num } });

    let res = set.join_next().await.unwrap().unwrap();
    assert_eq!(*res.val, 123);
}

#[tokio::test]
async fn multiple_borrowed_results() {
    let a = 10;
    let b = 20;

    let mut set = ScopedJoinSet::new();

    set.spawn(async { &a });
    set.spawn(async { &b });

    let mut results = vec![];
    while let Some(res) = set.join_next().await {
        results.push(*res.unwrap());
    }

    results.sort();
    assert_eq!(results, vec![10, 20]);
}

#[tokio::test]
async fn nested_borrow_result() {
    #[derive(Debug, PartialEq)]
    struct Outer<'a> {
        inner: Inner<'a>,
    }

    #[derive(Debug, PartialEq)]
    struct Inner<'a> {
        slice: &'a str,
    }

    let data = String::from("nested borrow");
    let slice = &data[0..6];

    let mut set = ScopedJoinSet::new();

    set.spawn(async {
        Outer {
            inner: Inner { slice },
        }
    });

    let res = set.join_next().await.unwrap().unwrap();
    assert_eq!(res.inner.slice, "nested");
}

#[tokio::test]
async fn borrowed_and_owned_mixed() {
    let text = String::from("mixed");
    let slice: &str = &text;

    let mut set = ScopedJoinSet::new();
    set.spawn(async { Cow::Borrowed(slice) });
    set.spawn(async { Cow::Owned("owned".to_string()) });

    let mut results = vec![];
    while let Some(result) = set.join_next().await {
        results.push(result.unwrap());
    }

    results.sort_by(|a, b| a.as_ref().cmp(b.as_ref()));

    assert_eq!(
        results,
        vec![Cow::Borrowed("mixed"), Cow::Owned("owned".to_string()),]
    );
}
