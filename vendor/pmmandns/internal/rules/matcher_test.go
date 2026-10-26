package rules

import "testing"

func TestHasSuffixBoundary(t *testing.T) {
	trie := newDomainTrie()
	trie.add("example.com", true, false)
	if !trie.match("www.example.com") {
		t.Fatal("expected match")
	}
	if trie.match("badexample.com") {
		t.Fatal("unexpected match")
	}
}

func TestFullRule(t *testing.T) {
	trie := newDomainTrie()
	trie.add("example.com", false, true)
	if !trie.match("example.com") || trie.match("www.example.com") {
		t.Fatal("full rule boundary failure")
	}
}
