use crate::check::reported;
use crate::scope::Scope;
use crate::workspace::Workspace;
use crate::workspace::tests::Scratch;

const MANIFEST: &str =
    "//! Native cases.\n\npackage app {\n    language 1\n    targets x86_64-host\n} app\n";

const CASES: &str = r#"//! Native cases.

fn open_channel(alloc: mem::Alloc) -> usize {
    let channel = task::Channel[u32].bounded(4, alloc)
    let extra = channel.sender.split()
    discard extra
    return 4
} open_channel

pub fn channel_charge(alloc: mem::Alloc) -> usize
    example channel_charge(testing::world().alloc) == 0
{
    let budget = alloc.split(65_536)
    discard open_channel(budget.alloc())
    let charge = budget.charged()
    take budget.close()
    return charge
} channel_charge

fn open_cown(alloc: mem::Alloc) -> usize {
    let cown = task::Cown.of(7u32, alloc)
    let other = cown.share()
    discard other
    return 1
} open_cown

pub fn cown_charge(alloc: mem::Alloc) -> usize
    example cown_charge(testing::world().alloc) == 0
{
    let budget = alloc.split(65_536)
    discard open_cown(budget.alloc())
    let charge = budget.charged()
    take budget.close()
    return charge
} cown_charge

pub fn probe() -> bool
    example (attempt sys::sim::fs(1_024, alloc: testing::world().alloc).read_only()).is_ok()
    example (attempt sys::sim::net().only_host("localhost")).is_ok()
    example not testing::same(sys::sim::stdio(b"in", alloc: testing::world().alloc), expected: sys::sim::stdio(b"", alloc: testing::world().alloc))
{
    return true
} probe
"#;

const REVOCATION: &str = r#"//! Revocation cases.

pub fn probe() -> bool
    example (attempt sys::revocable(testing::world().clock)).is_ok()
    example after_revoke(testing::world().clock) == Err(sys::Revoked.Stale)
    example before_revoke(testing::world().clock) == Ok(0)
    example attenuated_after_revoke(testing::world().fs)
    example handles(testing::world().clock)
{
    return true
} probe

pub fn after_revoke(clock: sys::Clock) -> Result[u64, sys::Revoked] {
    let Ok(revoker) = attempt sys::revocable(clock) else {
        return Ok(1)
    }
    let copy = revoker.capability()
    take revoker.revoke()
    return attempt copy.elapsed_ms()
} after_revoke

pub fn before_revoke(clock: sys::Clock) -> Result[u64, sys::Revoked] {
    let Ok(revoker) = attempt sys::revocable(clock) else {
        return Ok(1)
    }
    let copy = revoker.capability()
    let reading = attempt copy.elapsed_ms()
    take revoker.revoke()
    return reading
} before_revoke

pub fn attenuated_after_revoke(fs: sys::Fs) -> bool {
    let Ok(revoker) = attempt sys::revocable(fs) else {
        return false
    }
    let narrowed = attempt revoker.capability().read_only()
    take revoker.revoke()
    let Ok(narrow) = narrowed else {
        return false
    }
    return (attempt narrow.read_only()).is_err()
} attenuated_after_revoke

pub fn handles(clock: sys::Clock) -> bool {
    let Ok(revoker) = attempt sys::revocable(clock) else {
        return false
    }
    let one = revoker.capability()
    let second = revoker.capability()
    take revoker.revoke()
    let one_reading = attempt one.elapsed_ms()
    let second_reading = attempt second.elapsed_ms()
    return one_reading.is_err() and second_reading.is_err()
} handles
"#;

const PROJECTED: &str = r#"//! Projected option cases.

pub fn once_or_zero(alloc: mem::Alloc) -> u64
    example once_or_zero(testing::world().alloc) == 11
{
    let memo = mem::OnceMap[u32, u64].empty(seed: 0, alloc)
    discard memo.insert(1, value: 11)
    return memo.get(1).value_or(0)
} once_or_zero
"#;

/// The diagnostics of a package of cases.
fn diagnostics(cases: &str) -> Vec<String> {
    let scratch = Scratch::new(&[("app/package.nan", MANIFEST), ("app/app.nan", cases)]);
    let workspace = Workspace::load(scratch.0.clone(), &mut Vec::new()).expect("a workspace");
    reported(&workspace, &Scope::Workspace)
        .iter()
        .map(|diagnostic| format!("{diagnostic:?}"))
        .collect()
}

#[test]
fn channels_and_cowns_return_their_memory_and_sims_attenuate() {
    let found = diagnostics(CASES);
    assert!(found.is_empty(), "{}", found.join("\n"));
}

#[test]
fn revoking_a_capability_makes_it_and_what_derives_from_it_stale() {
    let found = diagnostics(REVOCATION);
    assert!(found.is_empty(), "{}", found.join("\n"));
}

#[test]
fn value_or_of_a_projected_option_gives_the_value_its_part_holds() {
    let found = diagnostics(PROJECTED);
    assert!(found.is_empty(), "{}", found.join("\n"));
}
