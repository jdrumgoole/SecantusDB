### Rust MongoDB server: resume tokens with type bits

#### Fixed

- **A resume token in mongod's format is read with its `_typeBits`**
  (RM-075 part), as mongod's `TypeBits` reader reads them. A number decodes
  as the type its bits name, a string takes its symbol bit, and zeros take
  their 3 and 12 bits. A whole-number or teeny decimal has its exponent
  rescaled by `adjustDecimalExponent`, transcribed with mongod's unsigned
  arithmetic. The answer is mongod's error wherever the bits make one,
  50615, 50813, 50814, 50830 and 50846 included. Sweeps of 695 and 1,384
  tokens found no difference
  (`tests/backlog/test_rm_057_084.py::test_type_bits_that_change_a_number`).
  A decimal rebuilt from a double is still RM-075.
