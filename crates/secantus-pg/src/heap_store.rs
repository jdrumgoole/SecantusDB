//! A heap kept across a restart (RP-193; decided by Joe, 2026-10-24: a
//! checkpointer and a reconcile). PostgreSQL keeps a table's pages and its
//! free space map on disk; here the heap model ([`crate::heap_page`]) is
//! written to the store as it changes -- at most once a second, at the end
//! of a message cycle and outside any user transaction, which is what
//! PostgreSQL's checkpointer and background writer do with dirty pages --
//! and at a clean shutdown. The first touch after a start reads it back
//! and reconciles it with the rows, whose stored ctids are the truth: a
//! crash can lose the last second's dead tuples and free space, never a
//! live row's place.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use bson::{doc, Bson, Document};

use crate::heap_page::{Heap, Lp, Page, Tuple, Xmax, LINE_POINTER, PAGE_SPACE};

/// Each database's saved heaps: a document per table, by its oid.
pub(crate) const COLLECTION: &str = "__sql_heap_pages__";

/// How often the end of a message cycle writes the heaps that changed.
const INTERVAL: Duration = Duration::from_secs(1);

fn i(v: impl Into<i64>) -> Bson {
    Bson::Int64(v.into())
}

fn lp_doc(off: u16, lp: &Lp) -> Bson {
    let mut v = vec![i(off)];
    match lp {
        Lp::Unused => v.push(Bson::String("u".into())),
        Lp::Dead => v.push(Bson::String("d".into())),
        Lp::Redirect(to) => {
            v.push(Bson::String("r".into()));
            v.push(i(*to));
        }
        Lp::Normal(t) => {
            v.push(Bson::String("n".into()));
            v.push(i(t.len));
            v.push(Bson::Boolean(t.heap_only));
            v.push(i(t.hot_next.map_or(-1, i64::from)));
            v.push(match &t.xmax {
                Some(x) => Bson::Document(doc! {
                    "xid": x.xid as i64,
                    "id": x.id.clone(),
                    "committed": x.committed,
                }),
                None => Bson::Null,
            });
            v.push(Bson::Boolean(t.xmin_aborted));
        }
    }
    Bson::Array(v)
}

fn lp_from(v: &[Bson]) -> Option<(u16, Lp)> {
    let int = |b: Option<&Bson>| b.and_then(crate::bson_i64);
    let off = u16::try_from(int(v.first())?).ok()?;
    let lp = match v.get(1)?.as_str()? {
        "u" => Lp::Unused,
        "d" => Lp::Dead,
        "r" => Lp::Redirect(u16::try_from(int(v.get(2))?).ok()?),
        "n" => Lp::Normal(Tuple {
            len: u32::try_from(int(v.get(2))?).ok()?,
            heap_only: v.get(3)?.as_bool()?,
            hot_next: u16::try_from(int(v.get(4))?).ok(),
            xmax: match v.get(5)? {
                Bson::Document(x) => Some(Xmax {
                    xid: x.get_i64("xid").ok()? as u64,
                    id: x.get("id").cloned().unwrap_or(Bson::Null),
                    committed: x.get_bool("committed").unwrap_or(false),
                }),
                _ => None,
            },
            xmin_aborted: v.get(6)?.as_bool()?,
        }),
        _ => return None,
    };
    Some((off, lp))
}

/// A heap as a document: its pages, the free space map, the pages a prune
/// may visit. Each backend's target block is not kept: it is PostgreSQL's
/// relcache, which a restart loses there too.
pub(crate) fn heap_doc(oid: i64, heap: &Heap) -> Document {
    let pages: Vec<Bson> = heap
        .pages
        .iter()
        .map(|p| {
            Bson::Document(doc! {
                "free": i(p.free),
                "nlp": i(p.nlp),
                "full": p.full,
                "prune": p.prune_xid.map_or(Bson::Null, |x| Bson::Int64(x as i64)),
                "lps": p.lps.iter().map(|(o, lp)| lp_doc(*o, lp)).collect::<Vec<_>>(),
            })
        })
        .collect();
    doc! {
        "_id": oid.to_string(),
        "reserve": i(heap.reserve),
        "disordered": heap.disordered,
        "prunable": heap.prunable.iter().map(|b| i(*b)).collect::<Vec<_>>(),
        "pages": pages,
        "fsm": heap.fsm.to_doc(),
    }
}

/// The heap [`heap_doc`] wrote.
pub(crate) fn heap_from(d: &Document) -> Heap {
    let mut heap = Heap {
        reserve: d.get_i64("reserve").unwrap_or(0).try_into().unwrap_or(0),
        disordered: d.get_bool("disordered").unwrap_or(false),
        ..Heap::default()
    };
    for b in d.get_array("prunable").map(|a| a.as_slice()).unwrap_or(&[]) {
        if let Some(b) = crate::bson_i64(b).and_then(|b| u32::try_from(b).ok()) {
            heap.prunable.insert(b);
        }
    }
    for p in d.get_array("pages").map(|a| a.as_slice()).unwrap_or(&[]) {
        let Bson::Document(p) = p else { continue };
        let mut page = Page {
            free: p.get_i64("free").unwrap_or(0).try_into().unwrap_or(0),
            nlp: p.get_i64("nlp").unwrap_or(0).try_into().unwrap_or(0),
            full: p.get_bool("full").unwrap_or(false),
            prune_xid: p.get_i64("prune").ok().map(|x| x as u64),
            ..Page::default()
        };
        for lp in p.get_array("lps").map(|a| a.as_slice()).unwrap_or(&[]) {
            if let Some((off, lp)) = lp.as_array().and_then(|v| lp_from(v)) {
                page.lps.insert(off, lp);
            }
        }
        heap.pages.push(page);
    }
    if let Ok(f) = d.get_document("fsm") {
        heap.fsm = crate::fsm::Fsm::from_doc(f);
    }
    heap
}

/// A saved heap made true to the rows, `(block, offset, length)` each live
/// one: the rows' ctids are what the store committed, the saved pages what
/// the last write knew.
///
/// - A saved tuple with an xmax whose row is still in its place had its
///   deleter roll back; one whose row is gone had it commit.
/// - A line pointer saved dead or unused where a live row now is was
///   reused after the save: it is that row's again.
/// - An ordinary place below a page's count that no row holds lost its row
///   after the save: a dead line pointer.
/// - A row past the saved pages was written after the save: its page grows.
pub(crate) fn reconcile(mut heap: Heap, rows: &[(u32, u16, u32)]) -> Heap {
    let mut live: HashMap<(u32, u16), u32> = HashMap::new();
    for (b, o, len) in rows {
        live.insert((*b, *o), *len);
        if heap.pages.len() <= *b as usize {
            heap.pages.resize(*b as usize + 1, Page::default());
            let page = &mut heap.pages[*b as usize];
            page.free = PAGE_SPACE;
        }
    }
    for (b, page) in heap.pages.iter_mut().enumerate() {
        let b = b as u32;
        let saved_nlp = page.nlp;
        for off in 1..=saved_nlp {
            let here = live.contains_key(&(b, off));
            match page.lps.get_mut(&off) {
                Some(Lp::Normal(t)) => match (&mut t.xmax, here) {
                    (Some(_), true) => t.xmax = None,
                    (Some(x), false) => x.committed = true,
                    (None, true) => {}
                    (None, false) => {
                        t.xmax = Some(Xmax {
                            xid: 0,
                            id: Bson::Null,
                            committed: true,
                        })
                    }
                },
                Some(_) if here => {
                    page.lps.remove(&off);
                }
                Some(_) => {}
                None if !here => {
                    page.lps.insert(off, Lp::Dead);
                }
                None => {}
            }
        }
        // Rows placed after the save: each takes its length, and the page
        // a line pointer for every slot up to the highest.
        let mut grown = 0;
        let mut top = saved_nlp;
        for ((rb, ro), len) in &live {
            if *rb == b && *ro > saved_nlp {
                grown += len;
                top = top.max(*ro);
            }
        }
        page.free = page
            .free
            .saturating_sub(grown + LINE_POINTER * u32::from(top - saved_nlp));
        page.nlp = top;
        for off in saved_nlp + 1..=page.nlp {
            if !live.contains_key(&(b, off)) {
                page.lps.insert(off, Lp::Dead);
            }
        }
    }
    heap.targets.clear();
    heap.inserting.clear();
    heap.inserting_by_xid.clear();
    heap
}

/// The last write of each store's heaps.
fn last_write() -> &'static Mutex<HashMap<usize, Instant>> {
    static LAST: OnceLock<Mutex<HashMap<usize, Instant>>> = OnceLock::new();
    LAST.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Is it time for `store`'s heaps to be written? (Now, when `force`.)
pub(crate) fn due(store: usize, force: bool) -> bool {
    let mut last = last_write().lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    if !force
        && last
            .get(&store)
            .is_some_and(|t| now.duration_since(*t) < INTERVAL)
    {
        return false;
    }
    last.insert(store, now);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(nlp: u16, lps: Vec<(u16, Lp)>) -> Page {
        Page {
            free: 8000,
            nlp,
            lps: lps.into_iter().collect(),
            ..Page::default()
        }
    }

    fn deleted(len: u32) -> Lp {
        Lp::Normal(Tuple {
            len,
            heap_only: false,
            hot_next: None,
            xmax: Some(Xmax {
                xid: 7,
                id: Bson::Int32(1),
                committed: false,
            }),
            xmin_aborted: false,
        })
    }

    #[test]
    fn a_saved_heap_round_trips() {
        let mut heap = Heap::default();
        heap.pages.push(page(
            3,
            vec![(1, Lp::Dead), (2, Lp::Redirect(3)), (3, deleted(40))],
        ));
        heap.prunable.insert(0);
        let back = heap_from(&heap_doc(16384, &heap));
        assert_eq!(back.pages[0].lps, heap.pages[0].lps);
        assert_eq!((back.pages[0].nlp, back.pages[0].free), (3, 8000));
        assert!(back.prunable.contains(&0));
    }

    #[test]
    fn the_rows_settle_what_the_save_did_not_know() {
        // Saved: (0,1) deleted by a transaction still running, (0,2) dead,
        // (0,3) an ordinary tuple.
        let mut heap = Heap::default();
        heap.pages
            .push(page(3, vec![(1, deleted(40)), (2, Lp::Dead)]));
        // Since: the delete of (0,1) rolled back, (0,2) was reused, the
        // row at (0,3) was deleted, and (0,4) and (1,1) were inserted.
        let rows = [(0, 1, 40), (0, 2, 40), (0, 4, 40), (1, 1, 40)];
        let h = reconcile(heap, &rows);
        assert!(matches!(&h.pages[0].lps[&1], Lp::Normal(t) if t.xmax.is_none()));
        assert!(!h.pages[0].lps.contains_key(&2));
        assert!(matches!(h.pages[0].lps.get(&3), Some(Lp::Dead)));
        assert_eq!(h.pages[0].nlp, 4);
        assert_eq!(h.pages[0].free, 8000 - 40 - 4);
        assert_eq!(h.pages.len(), 2);
        assert_eq!(h.pages[1].nlp, 1);
    }

    #[test]
    fn a_saved_delete_whose_row_is_gone_committed() {
        let mut heap = Heap::default();
        heap.pages.push(page(1, vec![(1, deleted(40))]));
        let h = reconcile(heap, &[]);
        assert!(
            matches!(&h.pages[0].lps[&1], Lp::Normal(t) if t.xmax.as_ref().is_some_and(|x| x.committed))
        );
    }
}
