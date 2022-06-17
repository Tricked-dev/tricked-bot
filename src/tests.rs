use select::{document::Document, predicate::Class};

// #[test]
// fn test_scrapping_ddg() {
//     let document = Document::from(include_str!("../tests/ddg.html"));
//     for node in document.find(Class("result__body")).take(10) {
//         let _url = node
//             .find(Class("result__a"))
//             .next()
//             .unwrap()
//             .attr("href")
//             .unwrap()
//             .replace("//duckduckgo.com", "https://duckduckgo.com");
//         let _snippet = node
//             .find(Class("result__snippet"))
//             .next()
//             .unwrap()
//             .inner_html()
//             .replace("<b>", "**")
//             .replace("</b>", "**")
//             .split_whitespace()
//             .collect::<Vec<&str>>()
//             .join(" ");
//         let icon = node
//             .find(Class("result__icon__img"))
//             .next()
//             .unwrap()
//             .attr("src")
//             .unwrap()
//             .replace(
//                 "//external-content.duckduckgo.com",
//                 "https://external-content.duckduckgo.com",
//             );

//         let url = node.find(Class("result__url")).next().unwrap().inner_html();
//         println!("{icon} {url}");
//     }
// }
