use uw_vk_layer::*;
#[test]
fn every_referencing_submission_must_complete_before_retirement() {
    let mut queue = RetireQueue::new(2);
    queue
        .retire(vec![3, 7], "mapping/image/descriptor")
        .unwrap();
    assert!(queue.sweep(|e| e == 3).is_empty());
    assert_eq!(
        queue.sweep(|e| e == 3 || e == 7),
        vec!["mapping/image/descriptor"]
    );
    assert!(queue.is_empty());
}
#[test]
fn capacity_is_bounded_and_same_numeric_handles_keep_distinct_generations() {
    let mut queue = RetireQueue::new(2);
    queue.retire(vec![1], (42, 1)).unwrap();
    queue.retire(vec![2], (42, 2)).unwrap();
    assert_eq!(queue.retire(vec![3], (42, 3)), Err((42, 3)));
    assert_eq!(queue.sweep(|e| e == 1), vec![(42, 1)]);
    assert_eq!(queue.len(), 1);
    assert_eq!(queue.sweep(|e| e == 1), vec![]);
    assert_eq!(queue.sweep(|e| e == 2), vec![(42, 2)]);
}
