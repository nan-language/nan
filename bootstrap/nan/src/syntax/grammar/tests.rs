use super::*;

#[test]
fn choices_are_ll1_under_the_published_resolutions() {
    for (choice, alts) in CHOICES.iter().enumerate() {
        for (i, &left) in alts.iter().enumerate() {
            for &right in &alts[i + 1..] {
                let (left, right) = (ANALYSIS.seq(left), ANALYSIS.seq(right));
                let shared: Vec<String> = left
                    .ok
                    .terms()
                    .filter(|term| right.ok.contains(*term))
                    .map(Term::written)
                    .collect();
                assert!(
                    shared.is_empty(),
                    "choice {choice}: alternatives share {shared:?}"
                );
                let rejected: Vec<String> = left
                    .rejected
                    .terms()
                    .filter(|term| right.rejected.contains(*term))
                    .map(Term::written)
                    .collect();
                assert!(
                    rejected.is_empty(),
                    "choice {choice}: rejected alternatives share {rejected:?}"
                );
            }
        }
    }
}

#[test]
fn rejected_code_points_leave_printable_ascii_alone() {
    assert!(
        REJECTED
            .iter()
            .all(|&(low, high)| high < 0x20 || low >= 0x7F)
    );
}
