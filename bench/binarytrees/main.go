package main

import "fmt"

type Node struct{ left, right *Node }

func make_(d int) *Node {
	if d == 0 {
		return &Node{}
	}
	return &Node{make_(d - 1), make_(d - 1)}
}

func check(n *Node) int {
	if n.left == nil {
		return 1
	}
	return 1 + check(n.left) + check(n.right)
}

func main() {
	max := 18
	fmt.Printf("stretch tree of depth %d\t check: %d\n", max+1, check(make_(max+1)))
	long := make_(max)
	for d := 4; d <= max; d += 2 {
		iters := 1 << (max - d + 4)
		c := 0
		for i := 0; i < iters; i++ {
			c += check(make_(d))
		}
		fmt.Printf("%d\t trees of depth %d\t check: %d\n", iters, d, c)
	}
	fmt.Printf("long lived tree of depth %d\t check: %d\n", max, check(long))
}
