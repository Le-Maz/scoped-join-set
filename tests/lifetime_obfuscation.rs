use std::borrow::Cow;
use scoped_join_set::scope;

#[tokio::test]
async fn borrowed_struct_result() {
    #[derive(Debug, PartialEq)]
    struct Borrowed<'a> {
        val: &'a i32,
    }

    let num = 123;
    
    scope(async |s| {
        s.spawn(async { Borrowed { val: &num } }).await;

        let res = s.join_next().await.unwrap().unwrap();
        assert_eq!(*res.val, 123);
    }).await;
}

#[tokio::test]
async fn multiple_borrowed_results() {
    let a = 10;
    let b = 20;

    scope(async |s| {
        s.spawn(async { &a }).await;
        s.spawn(async { &b }).await;

        let mut results = vec![];
        while let Some(res) = s.join_next().await {
            results.push(*res.unwrap());
        }

        results.sort();
        assert_eq!(results, vec![10, 20]);
    }).await;
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

    scope(async |s| {
        s.spawn(async {
            Outer {
                inner: Inner { slice },
            }
        }).await;

        let res = s.join_next().await.unwrap().unwrap();
        assert_eq!(res.inner.slice, "nested");
    }).await;
}

#[tokio::test]
async fn borrowed_and_owned_mixed() {
    let text = String::from("mixed");
    let slice: &str = &text;

    scope(async |s| {
        s.spawn(async { Cow::Borrowed(slice) }).await;
        s.spawn(async { Cow::Owned("owned".to_string()) }).await;

        let mut results = vec![];
        while let Some(result) = s.join_next().await {
            results.push(result.unwrap());
        }

        results.sort_by(|a, b| a.as_ref().cmp(b.as_ref()));

        assert_eq!(
            results,
            vec![Cow::Borrowed("mixed"), Cow::Owned("owned".to_string()),]
        );
    }).await;
}
