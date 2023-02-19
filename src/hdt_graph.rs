// //! *This module is available only if HDT is built with the `"sophia"` feature.*
#[cfg(feature = "sophia")]
use crate::four_sect_dict::IdKind;
use crate::hdt::Hdt;
use crate::triples::{Id, ObjectIter, PredicateIter, PredicateObjectIter, SubjectIter, TripleId};
use log::debug;

use sophia::api::graph::{GTripleSource, Graph};

use sophia::api::term::{matcher::TermMatcher, BnodeId, IriRef, LanguageTag, Term};
use std::convert::Infallible;
use std::io::{self, Error, ErrorKind};
use std::iter;
use std::sync::Arc;

mod term;
pub use term::HdtTerm;

/// Adapter to use HDT as a Sophia graph.
pub struct HdtGraph {
    /// Wrapped HDT instance
    pub hdt: Hdt,
}

impl HdtGraph {
    /// Wrapper around Hdt.
    pub const fn new(hdt: Hdt) -> Self {
        HdtGraph { hdt }
    }
    /// Size in bytes on the heap.
    pub fn size_in_bytes(&self) -> usize {
        self.hdt.size_in_bytes()
    }

    fn id_term(&self, id: Id, kind: &'static IdKind) -> HdtTerm {
        auto_term(self.hdt.dict.id_to_string(id, kind).unwrap().into()).unwrap()
        // TODO: optimize by excluding cases depending on the id kind
        //IriRef::new_unchecked(MownStr::from(s)).into_term()
    }

    /// Return
    /// - Ok(None) if tm is not a constant matcher
    /// - Ok(Some(term, term_id))) if tm is a constant matcher
    /// - Err(()) if tm is a constant matcher, but its only term is not in this graph
    fn unpack_matcher<T: TermMatcher>(&self, tm: T, kind: &IdKind) -> Result<Option<(HdtTerm, Id)>, ()> {
        match tm.constant() {
            Some(t) => match HdtTerm::try_from(t.borrow_term()) {
                Some(t) => {
                    let id = self.hdt.dict.string_to_id(&term_string(&t), kind);
                    if id == 0 {
                        return Err(());
                    }
                    Ok(Some((t, id)))
                }
                None => Err(()),
            },
            None => Ok(None),
        }
    }
}

/// Create the correct Sophia term for a given resource string.
/// Slow, use the appropriate method if you know which type (Literal, URI, or blank node) the string has.
fn auto_term(s: Arc<str>) -> io::Result<HdtTerm> {
    match s.chars().next() {
        None => Err(Error::new(ErrorKind::InvalidData, "empty input")),
        Some('"') => match s.rfind('"') {
            None => Err(Error::new(
                ErrorKind::InvalidData,
                format!("missing right quotation mark in literal string {s}"),
            )),
            Some(index) => {
                let lex = Arc::from(&s[1..index]);
                let rest = &s[index + 1..];
                // literal with no language tag and no datatype
                if rest.is_empty() {
                    return Ok(HdtTerm::LiteralDatatype(lex, term::XSD_STRING.clone()));
                }
                // either language tag or datatype
                if let Some(tag_index) = rest.find('@') {
                    let tag = LanguageTag::new_unchecked(Arc::from(&rest[tag_index + 1..]));
                    return Ok(HdtTerm::LiteralLanguage(lex, tag));
                }
                // datatype
                let mut dt_split = rest.split("^^");
                dt_split.next(); // empty
                match dt_split.next() {
                    Some(dt) => {
                        let unquoted = &dt[1..dt.len() - 1];
                        let dt = IriRef::new_unchecked(Arc::from(unquoted));
                        Ok(HdtTerm::LiteralDatatype(lex, dt))
                    }
                    None => Err(Error::new(ErrorKind::InvalidData, format!("empty datatype in {s}"))),
                }
            }
        },
        Some('_') => Ok(HdtTerm::BlankNode(BnodeId::new_unchecked(Arc::from(&s[2..])))),
        _ => Ok(HdtTerm::Iri(IriRef::new_unchecked(Arc::from(&s[..])))),
    }
}

// Convert a SimpleTerm into the HDT String format.
// Sophia doesn't include the _: prefix for blank node strings but HDT expects it
// not needed for property terms, as they can't be blank nodes
fn term_string(t: &HdtTerm) -> String {
    match t {
        HdtTerm::BlankNode(b) => "_:".to_owned() + b.as_str(),
        HdtTerm::Iri(i) => i.as_str().to_owned(),
        HdtTerm::LiteralLanguage(l, lang) => {
            format!("\"{l}\"@{}", lang.as_str())
        }
        HdtTerm::LiteralDatatype(l, dt) => {
            let xsd_string: &str = "http://www.w3.org/2001/XMLSchema#string";
            let dts = dt.as_str();
            if dts == xsd_string {
                format!("\"{l}\"")
            } else {
                format!("\"{l}\"^^<{dts}>")
            }
        }
    }
}

impl Graph for HdtGraph {
    type Triple<'a> = [HdtTerm; 3];
    type Error = Infallible; // infallible for now, figure out what to put here later

    fn triples(&self) -> GTripleSource<Self> {
        debug!("Iterating through ALL triples in the HDT Graph. This can be inefficient for large graphs.");
        Box::new(self.hdt.triples().map(move |(s, p, o)| {
            Ok([auto_term(s).unwrap(), HdtTerm::Iri(IriRef::new_unchecked(p)), auto_term(o).unwrap()])
        }))
    }

    /// Only supports constant and "any" matchers.
    /// Non-constant matchers are supposed to be "any" matchers.
    fn triples_matching<'s, S, P, O>(&'s self, sm: S, pm: P, om: O) -> GTripleSource<'s, Self>
    where
        S: TermMatcher + 's,
        P: TermMatcher + 's,
        O: TermMatcher + 's,
    {
        let xso = match self.unpack_matcher(sm, &IdKind::Subject) {
            Err(_) => return Box::new(iter::empty()),
            Ok(x) => x,
        };
        let xpo = match self.unpack_matcher(pm, &IdKind::Predicate) {
            Err(_) => return Box::new(iter::empty()),
            Ok(x) => x,
        };
        let xoo = match self.unpack_matcher(om, &IdKind::Object) {
            Err(_) => return Box::new(iter::empty()),
            Ok(x) => x,
        };
        // TODO: improve error handling
        match (xso, xpo, xoo) {
            (Some(s), Some(p), Some(o)) => {
                if SubjectIter::with_pattern(&self.hdt.triples, &TripleId::new(s.1, p.1, o.1)).next().is_some() {
                    Box::new(iter::once(Ok([s.0, p.0, o.0])))
                } else {
                    Box::new(iter::empty())
                }
            }
            (Some(s), Some(p), None) => {
                Box::new(SubjectIter::with_pattern(&self.hdt.triples, &TripleId::new(s.1, p.1, 0)).map(move |t| {
                    Ok([
                        s.0.clone(),
                        p.0.clone(),
                        auto_term(self.hdt.dict.id_to_string(t.object_id, &IdKind::Object).unwrap().into())
                            .unwrap(),
                    ])
                }))
            }
            (Some(s), None, Some(o)) => {
                Box::new(SubjectIter::with_pattern(&self.hdt.triples, &TripleId::new(s.1, 0, o.1)).map(move |t| {
                    Ok([s.0.clone(), self.id_term(t.predicate_id, &IdKind::Predicate), o.0.clone()])
                }))
            }
            (Some(s), None, None) => {
                Box::new(SubjectIter::with_pattern(&self.hdt.triples, &TripleId::new(s.1, 0, 0)).map(move |t| {
                    Ok([
                        s.0.clone(),
                        self.id_term(t.predicate_id, &IdKind::Predicate),
                        auto_term(Arc::from(self.hdt.dict.id_to_string(t.object_id, &IdKind::Object).unwrap()))
                            .expect("auto term failed with object"),
                    ])
                }))
            }
            (None, Some(p), Some(o)) => Box::new(
                PredicateObjectIter::new(&self.hdt.triples, p.1, o.1)
                    .map(move |sid| Ok([self.id_term(sid, &IdKind::Subject), p.0.clone(), o.0.clone()])),
            ),
            (None, Some(p), None) => Box::new(PredicateIter::new(&self.hdt.triples, p.1).map(move |t| {
                Ok([
                    self.id_term(t.subject_id, &IdKind::Subject),
                    p.0.clone(),
                    self.id_term(t.object_id, &IdKind::Object),
                ])
            })),
            (None, None, Some(o)) => Box::new(ObjectIter::new(&self.hdt.triples, o.1).map(move |t| {
                Ok([
                    auto_term(Arc::from(self.hdt.dict.id_to_string(t.subject_id, &IdKind::Subject).unwrap()))
                        .unwrap(),
                    self.id_term(t.predicate_id, &IdKind::Predicate),
                    o.0.clone(),
                ])
            })),
            (None, None, None) => self.triples(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::init;
    use sophia::api::prelude::Triple;
    use sophia::api::term::matcher::Any;
    use std::fs::File;
    use std::result::Result;

    #[test]
    fn test_graph() {
        init();
        let file = File::open("tests/resources/snikmeta.hdt").expect("error opening file");
        let hdt = Hdt::new(std::io::BufReader::new(file)).unwrap();
        let graph = HdtGraph::new(hdt);
        let triples: Vec<Result<[HdtTerm; 3], Infallible>> = graph.triples().collect();
        assert_eq!(triples.len(), 327);
        let meta_top = "http://www.snik.eu/ontology/meta/Top";
        assert!(graph
            .triples_matching(
                Some(HdtTerm::Iri(IriRef::new_unchecked(Arc::from("http://www.snik.eu/ontology/meta")))),
                Any,
                Any
            )
            .next()
            .is_some());
        for uri in [meta_top, "http://www.snik.eu/ontology/meta", "doesnotexist"] {
            let term = HdtTerm::Iri(IriRef::new_unchecked(Arc::from(uri)));
            let filtered: Vec<_> = triples
                .iter()
                .map(|triple| triple.as_ref().unwrap())
                .filter(|triple| triple.s().iri().is_some() && triple.s().iri().unwrap().to_string() == uri)
                .collect();
            let with_s: Vec<_> = graph.triples_matching(Some(term), Any, Any).map(Result::unwrap).collect();
            // Sophia strings can't be compared directly, use the Debug trait for string comparison that is more brittle and less elegant
            // could break in the future e.g. because of ordering
            let filtered_string = format!("{filtered:?}");
            let with_s_string = format!("{with_s:?}");
            assert_eq!(
                filtered_string, with_s_string,
                "different results between triples() and triples_with_s() for {uri}"
            );
        }
        let s = HdtTerm::Iri(IriRef::new_unchecked(meta_top.into()));
        let p = HdtTerm::Iri(IriRef::new_unchecked("http://www.w3.org/2000/01/rdf-schema#label".into()));
        let o = HdtTerm::LiteralLanguage("top class".into(), LanguageTag::new_unchecked("en".into()));
        assert!(graph.triples_matching(Any, Any, [o.borrow_term()]).next().is_some());

        let tvec = vec![[s.clone(), p.clone(), o.clone()]];
        assert_eq!(
            tvec,
            graph
                .triples_matching([s.borrow_term()], [p.borrow_term()], Any)
                .map(Result::unwrap)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            tvec,
            graph
                .triples_matching([s.borrow_term()], Any, [o.borrow_term()])
                .map(Result::unwrap)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            tvec,
            graph
                .triples_matching(Any, [p.borrow_term()], [o.borrow_term()])
                .map(Result::unwrap)
                .collect::<Vec<_>>()
        );
        assert_eq!(1, graph.triples_matching(Any, Any, ["22.10"]).count());
        let date = HdtTerm::LiteralDatatype(
            "2022-10-20".into(),
            IriRef::new_unchecked("http://www.w3.org/2001/XMLSchema#date".into()),
        );
        assert_eq!(1, graph.triples_matching(Any, Any, Some(date)).count());
        // test for errors involving blank nodes
        let blank = HdtTerm::BlankNode(BnodeId::new_unchecked("b1".into()));
        // blank node as input
        assert_eq!(3, graph.triples_matching(Some(&blank), Any, Any).count());
        assert_eq!(1, graph.triples_matching(Any, Any, Some(&blank)).count());
        // blank node as output
        let rdftype =
            HdtTerm::Iri(IriRef::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type".into()));
        let owlrestriction =
            HdtTerm::Iri(IriRef::new_unchecked("http://www.w3.org/2002/07/owl#Restriction".into()));
        assert_eq!(1, graph.triples_matching(Any, Some(rdftype), Some(owlrestriction)).count());
        //let method = "http://www.snik.eu/ontology/meta/Method";
        // not in snik meta but only in local test file to make sure explicit xsd:string works
        /*
        let testo = &SimpleTerm::from(LiteralDatatype(
            "testo",
            Iri::<&str>::new_unchecked("http://www.w3.org/2001/XMLSchema#string"),
        ));
        assert!(graph.triples_matching(testo).count() == 1);
        */
    }
}
