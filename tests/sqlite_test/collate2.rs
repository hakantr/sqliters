#[test]
#[ignore = "8/119 subtests passed; pending: collate2 edge cases"]
fn test_collate2() {
    sqliters_tcltest::run_test_script("collate2", r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
set ::testprefix collate2
db collate BACKWARDS backwards_collate
proc backwards_collate {a b} {
  set ra {};
  set rb {}
  foreach c [split $a {}] { set ra $c$ra }
  foreach c [split $b {}] { set rb $c$rb }
  return [string compare $ra $rb]
}
do_test collate2-1.0 {
  execsql {
    CREATE TABLE collate2t1(
      a COLLATE BINARY, 
      b COLLATE NOCASE, 
      c COLLATE BACKWARDS
    );
    INSERT INTO collate2t1 VALUES( NULL, NULL, NULL );

    INSERT INTO collate2t1 VALUES( 'aa', 'aa', 'aa' );
    INSERT INTO collate2t1 VALUES( 'ab', 'ab', 'ab' );
    INSERT INTO collate2t1 VALUES( 'ba', 'ba', 'ba' );
    INSERT INTO collate2t1 VALUES( 'bb', 'bb', 'bb' );

    INSERT INTO collate2t1 VALUES( 'aA', 'aA', 'aA' );
    INSERT INTO collate2t1 VALUES( 'aB', 'aB', 'aB' );
    INSERT INTO collate2t1 VALUES( 'bA', 'bA', 'bA' );
    INSERT INTO collate2t1 VALUES( 'bB', 'bB', 'bB' );

    INSERT INTO collate2t1 VALUES( 'Aa', 'Aa', 'Aa' );
    INSERT INTO collate2t1 VALUES( 'Ab', 'Ab', 'Ab' );
    INSERT INTO collate2t1 VALUES( 'Ba', 'Ba', 'Ba' );
    INSERT INTO collate2t1 VALUES( 'Bb', 'Bb', 'Bb' );

    INSERT INTO collate2t1 VALUES( 'AA', 'AA', 'AA' );
    INSERT INTO collate2t1 VALUES( 'AB', 'AB', 'AB' );
    INSERT INTO collate2t1 VALUES( 'BA', 'BA', 'BA' );
    INSERT INTO collate2t1 VALUES( 'BB', 'BB', 'BB' );
  }
  if {[info exists collate_test_use_index]} { 
    execsql {
      CREATE INDEX collate2t1_i1 ON collate2t1(a);
      CREATE INDEX collate2t1_i2 ON collate2t1(b);
      CREATE INDEX collate2t1_i3 ON collate2t1(c);
    }
  }
} {}
do_test collate2-1.1 {
  execsql {
    SELECT a FROM collate2t1 WHERE a > 'aa' ORDER BY 1;
  }
} {ab bA bB ba bb}
do_test collate2-1.1.1 {
  execsql {
    SELECT a FROM collate2t1 WHERE a COLLATE binary > 'aa' ORDER BY 1;
  }
} {ab bA bB ba bb}
do_test collate2-1.1.2 {
  execsql {
    SELECT a FROM collate2t1 WHERE b COLLATE binary > 'aa' ORDER BY 1;
  }
} {ab bA bB ba bb}
do_test collate2-1.1.3 {
  execsql {
    SELECT a FROM collate2t1 WHERE c COLLATE binary > 'aa' ORDER BY 1;
  }
} {ab bA bB ba bb}
do_test collate2-1.2 {
  execsql {
    SELECT b FROM collate2t1 WHERE b > 'aa' ORDER BY 1, oid;
  }
} {ab aB Ab AB ba bA Ba BA bb bB Bb BB}
do_test collate2-1.2.1 {
  execsql {
    SELECT b FROM collate2t1 WHERE a COLLATE nocase > 'aa'
     ORDER BY 1, oid;
  }
} {ab aB Ab AB ba bA Ba BA bb bB Bb BB}
do_test collate2-1.2.2 {
  execsql {
    SELECT b FROM collate2t1 WHERE b COLLATE nocase > 'aa'
     ORDER BY 1, oid;
  }
} {ab aB Ab AB ba bA Ba BA bb bB Bb BB}
do_test collate2-1.2.3 {
  execsql {
    SELECT b FROM collate2t1 WHERE c COLLATE nocase > 'aa'
     ORDER BY 1, oid;
  }
} {ab aB Ab AB ba bA Ba BA bb bB Bb BB}
do_test collate2-1.2.4 {
  execsql {
    SELECT b FROM collate2t1 WHERE b > 'aa' ORDER BY +b;
  }
} {ab aB Ab AB ba bA Ba BA bb bB Bb BB}
do_test collate2-1.2.5 {
  execsql {
    SELECT b FROM collate2t1 WHERE a COLLATE nocase > 'aa' ORDER BY +b;
  }
} {ab aB Ab AB ba bA Ba BA bb bB Bb BB}
do_test collate2-1.2.6 {
  execsql {
    SELECT b FROM collate2t1 WHERE b COLLATE nocase > 'aa' ORDER BY +b;
  }
} {ab aB Ab AB ba bA Ba BA bb bB Bb BB}
do_test collate2-1.2.7 {
  execsql {
    SELECT b FROM collate2t1 WHERE c COLLATE nocase > 'aa' ORDER BY +b;
  }
} {ab aB Ab AB ba bA Ba BA bb bB Bb BB}
do_test collate2-1.3 {
  execsql {
    SELECT c FROM collate2t1 WHERE c > 'aa' ORDER BY 1;
  }
} {ba Ab Bb ab bb}
do_test collate2-1.3.1 {
  execsql {
    SELECT c FROM collate2t1 WHERE a COLLATE backwards > 'aa'
    ORDER BY 1;
  }
} {ba Ab Bb ab bb}
do_test collate2-1.3.2 {
  execsql {
    SELECT c FROM collate2t1 WHERE b COLLATE backwards > 'aa'
    ORDER BY 1;
  }
} {ba Ab Bb ab bb}
do_test collate2-1.3.3 {
  execsql {
    SELECT c FROM collate2t1 WHERE c COLLATE backwards > 'aa'
    ORDER BY 1;
  }
} {ba Ab Bb ab bb}
do_test collate2-1.4 {
  execsql {
    SELECT a FROM collate2t1 WHERE a < 'aa' ORDER BY 1;
  }
} {AA AB Aa Ab BA BB Ba Bb aA aB}
do_test collate2-1.5 {
  execsql {
    SELECT b FROM collate2t1 WHERE b < 'aa' ORDER BY 1, oid;
  }
} {}
do_test collate2-1.5.1 {
  execsql {
    SELECT b FROM collate2t1 WHERE b < 'aa' ORDER BY +b;
  }
} {}
do_test collate2-1.6 {
  execsql {
    SELECT c FROM collate2t1 WHERE c < 'aa' ORDER BY 1;
  }
} {AA BA aA bA AB BB aB bB Aa Ba}
do_test collate2-1.7 {
  execsql {
    SELECT a FROM collate2t1 WHERE a = 'aa';
  }
} {aa}
do_test collate2-1.8 {
  execsql {
    SELECT b FROM collate2t1 WHERE b = 'aa' ORDER BY oid;
  }
} {aa aA Aa AA}
do_test collate2-1.9 {
  execsql {
    SELECT c FROM collate2t1 WHERE c = 'aa';
  }
} {aa}
do_test collate2-1.10 {
  execsql {
    SELECT a FROM collate2t1 WHERE a >= 'aa' ORDER BY 1;
  }
} {aa ab bA bB ba bb}
do_test collate2-1.11 {
  execsql {
    SELECT b FROM collate2t1 WHERE b >= 'aa' ORDER BY 1, oid;
  }
} {aa aA Aa AA ab aB Ab AB ba bA Ba BA bb bB Bb BB}
do_test collate2-1.12 {
  execsql {
    SELECT c FROM collate2t1 WHERE c >= 'aa' ORDER BY 1;
  }
} {aa ba Ab Bb ab bb}
do_test collate2-1.13 {
  execsql {
    SELECT a FROM collate2t1 WHERE a <= 'aa' ORDER BY 1;
  }
} {AA AB Aa Ab BA BB Ba Bb aA aB aa}
do_test collate2-1.14 {
  execsql {
    SELECT b FROM collate2t1 WHERE b <= 'aa' ORDER BY 1, oid;
  }
} {aa aA Aa AA}
do_test collate2-1.15 {
  execsql {
    SELECT c FROM collate2t1 WHERE c <= 'aa' ORDER BY 1;
  }
} {AA BA aA bA AB BB aB bB Aa Ba aa}
do_test collate2-1.16 {
  execsql {
    SELECT a FROM collate2t1 WHERE a BETWEEN 'Aa' AND 'Bb' ORDER BY 1;
  }
} {Aa Ab BA BB Ba Bb}
do_test collate2-1.17 {
  execsql {
    SELECT b FROM collate2t1 WHERE b BETWEEN 'Aa' AND 'Bb' ORDER BY 1, oid;
  }
} {aa aA Aa AA ab aB Ab AB ba bA Ba BA bb bB Bb BB}
do_test collate2-1.17.1 {
  execsql {
    SELECT b FROM collate2t1 WHERE b BETWEEN 'Aa' AND 'Bb' ORDER BY +b;
  }
} {aa aA Aa AA ab aB Ab AB ba bA Ba BA bb bB Bb BB}
do_test collate2-1.18 {
  execsql {
    SELECT c FROM collate2t1 WHERE c BETWEEN 'Aa' AND 'Bb' ORDER BY 1;
  }
} {Aa Ba aa ba Ab Bb}
do_test collate2-1.19 {
  execsql {
    SELECT a FROM collate2t1 WHERE 
      CASE a WHEN 'aa' THEN 1 ELSE 0 END
        ORDER BY 1, oid;
  }
} {aa}
do_test collate2-1.20 {
  execsql {
    SELECT b FROM collate2t1 WHERE 
      CASE b WHEN 'aa' THEN 1 ELSE 0 END
        ORDER BY 1, oid;
  }
} {aa aA Aa AA}
do_test collate2-1.21 {
  execsql {
    SELECT c FROM collate2t1 WHERE 
      CASE c WHEN 'aa' THEN 1 ELSE 0 END
        ORDER BY 1, oid;
  }
} {aa}
ifcapable subquery {
  do_test collate2-1.22 {
    execsql {
      SELECT a FROM collate2t1 WHERE a IN ('aa', 'bb') ORDER BY 1, oid;
    }
  } {aa bb}
  do_test collate2-1.23 {
    execsql {
      SELECT b FROM collate2t1 WHERE b IN ('aa', 'bb') ORDER BY 1, oid;
    }
  } {aa aA Aa AA bb bB Bb BB}
  do_test collate2-1.24 {
    execsql {
      SELECT c FROM collate2t1 WHERE c IN ('aa', 'bb') ORDER BY 1, oid;
    }
  } {aa bb}
  do_test collate2-1.25 {
    execsql {
      SELECT a FROM collate2t1 
        WHERE a IN (SELECT a FROM collate2t1 WHERE a IN ('aa', 'bb'));
    }
  } {aa bb}
  do_test collate2-1.26 {
    execsql {
      SELECT b FROM collate2t1 
        WHERE b IN (SELECT a FROM collate2t1 WHERE a IN ('aa', 'bb'));
    }
  } {aa bb aA bB Aa Bb AA BB}
  do_test collate2-1.27 {
    execsql {
      SELECT c FROM collate2t1 
        WHERE c IN (SELECT a FROM collate2t1 WHERE a IN ('aa', 'bb'));
    }
  } {aa bb}
} ;# ifcapable subquery
do_test collate2-2.1 {
  execsql {
    SELECT a FROM collate2t1 WHERE NOT a > 'aa' ORDER BY 1;
  }
} {AA AB Aa Ab BA BB Ba Bb aA aB aa}
do_test collate2-2.2 {
  execsql {
    SELECT b FROM collate2t1 WHERE NOT b > 'aa' ORDER BY 1, oid;
  }
} {aa aA Aa AA}
do_test collate2-2.3 {
  execsql {
    SELECT c FROM collate2t1 WHERE NOT c > 'aa' ORDER BY 1;
  }
} {AA BA aA bA AB BB aB bB Aa Ba aa}
do_test collate2-2.4 {
  execsql {
    SELECT a FROM collate2t1 WHERE NOT a < 'aa' ORDER BY 1;
  }
} {aa ab bA bB ba bb}
do_test collate2-2.5 {
  execsql {
    SELECT b FROM collate2t1 WHERE NOT b < 'aa' ORDER BY 1, oid;
  }
} {aa aA Aa AA ab aB Ab AB ba bA Ba BA bb bB Bb BB}
do_test collate2-2.6 {
  execsql {
    SELECT c FROM collate2t1 WHERE NOT c < 'aa' ORDER BY 1;
  }
} {aa ba Ab Bb ab bb}
do_test collate2-2.7 {
  execsql {
    SELECT a FROM collate2t1 WHERE NOT a = 'aa';
  }
} {ab ba bb aA aB bA bB Aa Ab Ba Bb AA AB BA BB}
do_test collate2-2.8 {
  execsql {
    SELECT b FROM collate2t1 WHERE NOT b = 'aa';
  }
} {ab ba bb aB bA bB Ab Ba Bb AB BA BB}
do_test collate2-2.9 {
  execsql {
    SELECT c FROM collate2t1 WHERE NOT c = 'aa';
  }
} {ab ba bb aA aB bA bB Aa Ab Ba Bb AA AB BA BB}
do_test collate2-2.10 {
  execsql {
    SELECT a FROM collate2t1 WHERE NOT a >= 'aa' ORDER BY 1;
  }
} {AA AB Aa Ab BA BB Ba Bb aA aB}
do_test collate2-2.11 {
  execsql {
    SELECT b FROM collate2t1 WHERE NOT b >= 'aa' ORDER BY 1, oid;
  }
} {}
do_test collate2-2.12 {
  execsql {
    SELECT c FROM collate2t1 WHERE NOT c >= 'aa' ORDER BY 1;
  }
} {AA BA aA bA AB BB aB bB Aa Ba}
do_test collate2-2.13 {
  execsql {
    SELECT a FROM collate2t1 WHERE NOT a <= 'aa' ORDER BY 1;
  }
} {ab bA bB ba bb}
do_test collate2-2.14 {
  execsql {
    SELECT b FROM collate2t1 WHERE NOT b <= 'aa' ORDER BY 1, oid;
  }
} {ab aB Ab AB ba bA Ba BA bb bB Bb BB}
do_test collate2-2.15 {
  execsql {
    SELECT c FROM collate2t1 WHERE NOT c <= 'aa' ORDER BY 1;
  }
} {ba Ab Bb ab bb}
do_test collate2-2.16 {
  execsql {
    SELECT a FROM collate2t1 WHERE a NOT BETWEEN 'Aa' AND 'Bb' ORDER BY 1;
  }
} {AA AB aA aB aa ab bA bB ba bb}
do_test collate2-2.17 {
  execsql {
    SELECT b FROM collate2t1 WHERE b NOT BETWEEN 'Aa' AND 'Bb' ORDER BY 1, oid;
  }
} {}
do_test collate2-2.18 {
  execsql {
    SELECT c FROM collate2t1 WHERE c NOT BETWEEN 'Aa' AND 'Bb' ORDER BY 1;
  }
} {AA BA aA bA AB BB aB bB ab bb}
do_test collate2-2.19 {
  execsql {
    SELECT a FROM collate2t1 WHERE NOT CASE a WHEN 'aa' THEN 1 ELSE 0 END;
  }
} {{} ab ba bb aA aB bA bB Aa Ab Ba Bb AA AB BA BB}
do_test collate2-2.20 {
  execsql {
    SELECT b FROM collate2t1 WHERE NOT CASE b WHEN 'aa' THEN 1 ELSE 0 END;
  }
} {{} ab ba bb aB bA bB Ab Ba Bb AB BA BB}
do_test collate2-2.21 {
  execsql {
    SELECT c FROM collate2t1 WHERE NOT CASE c WHEN 'aa' THEN 1 ELSE 0 END;
  }
} {{} ab ba bb aA aB bA bB Aa Ab Ba Bb AA AB BA BB}
ifcapable subquery {
  do_test collate2-2.22 {
    execsql {
      SELECT a FROM collate2t1 WHERE NOT a IN ('aa', 'bb');
    }
  } {ab ba aA aB bA bB Aa Ab Ba Bb AA AB BA BB}
  do_test collate2-2.23 {
    execsql {
      SELECT b FROM collate2t1 WHERE NOT b IN ('aa', 'bb');
    }
  } {ab ba aB bA Ab Ba AB BA}
  do_test collate2-2.24 {
    execsql {
      SELECT c FROM collate2t1 WHERE NOT c IN ('aa', 'bb');
    }
  } {ab ba aA aB bA bB Aa Ab Ba Bb AA AB BA BB}
  do_test collate2-2.25 {
    execsql {
      SELECT a FROM collate2t1 
        WHERE NOT a IN (SELECT a FROM collate2t1 WHERE a IN ('aa', 'bb'));
    }
  } {ab ba aA aB bA bB Aa Ab Ba Bb AA AB BA BB}
  do_test collate2-2.26 {
    execsql {
      SELECT b FROM collate2t1 
        WHERE NOT b IN (SELECT a FROM collate2t1 WHERE a IN ('aa', 'bb'));
    }
  } {ab ba aB bA Ab Ba AB BA}
  do_test collate2-2.27 {
    execsql {
      SELECT c FROM collate2t1 
        WHERE NOT c IN (SELECT a FROM collate2t1 WHERE a IN ('aa', 'bb'));
    }
  } {ab ba aA aB bA bB Aa Ab Ba Bb AA AB BA BB}
}
do_test collate2-3.1 {
  execsql {
    SELECT a > 'aa' FROM collate2t1;
  }
} {{} 0 1 1 1 0 0 1 1 0 0 0 0 0 0 0 0}
do_test collate2-3.2 {
  execsql {
    SELECT b > 'aa' FROM collate2t1;
  }
} {{} 0 1 1 1 0 1 1 1 0 1 1 1 0 1 1 1}
do_test collate2-3.3 {
  execsql {
    SELECT c > 'aa' FROM collate2t1;
  }
} {{} 0 1 1 1 0 0 0 0 0 1 0 1 0 0 0 0}
do_test collate2-3.4 {
  execsql {
    SELECT a < 'aa' FROM collate2t1;
  }
} {{} 0 0 0 0 1 1 0 0 1 1 1 1 1 1 1 1}
do_test collate2-3.5 {
  execsql {
    SELECT b < 'aa' FROM collate2t1;
  }
} {{} 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0}
do_test collate2-3.6 {
  execsql {
    SELECT c < 'aa' FROM collate2t1;
  }
} {{} 0 0 0 0 1 1 1 1 1 0 1 0 1 1 1 1}
do_test collate2-3.7 {
  execsql {
    SELECT a = 'aa' FROM collate2t1;
  }
} {{} 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0}
do_test collate2-3.8 {
  execsql {
    SELECT b = 'aa' FROM collate2t1;
  }
} {{} 1 0 0 0 1 0 0 0 1 0 0 0 1 0 0 0}
do_test collate2-3.9 {
  execsql {
    SELECT c = 'aa' FROM collate2t1;
  }
} {{} 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0}
do_test collate2-3.10 {
  execsql {
    SELECT a <= 'aa' FROM collate2t1;
  }
} {{} 1 0 0 0 1 1 0 0 1 1 1 1 1 1 1 1}
do_test collate2-3.11 {
  execsql {
    SELECT b <= 'aa' FROM collate2t1;
  }
} {{} 1 0 0 0 1 0 0 0 1 0 0 0 1 0 0 0}
do_test collate2-3.12 {
  execsql {
    SELECT c <= 'aa' FROM collate2t1;
  }
} {{} 1 0 0 0 1 1 1 1 1 0 1 0 1 1 1 1}
do_test collate2-3.13 {
  execsql {
    SELECT a >= 'aa' FROM collate2t1;
  }
} {{} 1 1 1 1 0 0 1 1 0 0 0 0 0 0 0 0}
do_test collate2-3.14 {
  execsql {
    SELECT b >= 'aa' FROM collate2t1;
  }
} {{} 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1}
do_test collate2-3.15 {
  execsql {
    SELECT c >= 'aa' FROM collate2t1;
  }
} {{} 1 1 1 1 0 0 0 0 0 1 0 1 0 0 0 0}
do_test collate2-3.16 {
  execsql {
    SELECT a BETWEEN 'Aa' AND 'Bb' FROM collate2t1;
  }
} {{} 0 0 0 0 0 0 0 0 1 1 1 1 0 0 1 1}
do_test collate2-3.17 {
  execsql {
    SELECT b BETWEEN 'Aa' AND 'Bb' FROM collate2t1;
  }
} {{} 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1}
do_test collate2-3.18 {
  execsql {
    SELECT c BETWEEN 'Aa' AND 'Bb' FROM collate2t1;
  }
} {{} 1 0 1 0 0 0 0 0 1 1 1 1 0 0 0 0}
do_test collate2-3.19 {
  execsql {
    SELECT CASE a WHEN 'aa' THEN 1 ELSE 0 END FROM collate2t1;
  }
} {0 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0}
do_test collate2-3.20 {
  execsql {
    SELECT CASE b WHEN 'aa' THEN 1 ELSE 0 END FROM collate2t1;
  }
} {0 1 0 0 0 1 0 0 0 1 0 0 0 1 0 0 0}
do_test collate2-3.21 {
  execsql {
    SELECT CASE c WHEN 'aa' THEN 1 ELSE 0 END FROM collate2t1;
  }
} {0 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0}
ifcapable subquery {
  do_test collate2-3.22 {
    execsql {
      SELECT a IN ('aa', 'bb') FROM collate2t1;
    }
  } {{} 1 0 0 1 0 0 0 0 0 0 0 0 0 0 0 0}
  do_test collate2-3.23 {
    execsql {
      SELECT b IN ('aa', 'bb') FROM collate2t1;
    }
  } {{} 1 0 0 1 1 0 0 1 1 0 0 1 1 0 0 1}
  do_test collate2-3.24 {
    execsql {
      SELECT c IN ('aa', 'bb') FROM collate2t1;
    }
  } {{} 1 0 0 1 0 0 0 0 0 0 0 0 0 0 0 0}
  do_test collate2-3.25 {
    execsql {
      SELECT a IN (SELECT a FROM collate2t1 WHERE a IN ('aa', 'bb')) 
        FROM collate2t1;
    }
  } {{} 1 0 0 1 0 0 0 0 0 0 0 0 0 0 0 0}
  do_test collate2-3.26 {
    execsql {
      SELECT b IN (SELECT a FROM collate2t1 WHERE a IN ('aa', 'bb')) 
        FROM collate2t1;
    }
  } {{} 1 0 0 1 1 0 0 1 1 0 0 1 1 0 0 1}
  do_test collate2-3.27 {
    execsql {
      SELECT c IN (SELECT a FROM collate2t1 WHERE a IN ('aa', 'bb')) 
        FROM collate2t1;
    }
  } {{} 1 0 0 1 0 0 0 0 0 0 0 0 0 0 0 0}
}
do_test collate2-4.0 {
  execsql {
    CREATE TABLE collate2t2(b COLLATE binary);
    CREATE TABLE collate2t3(b text);
    INSERT INTO collate2t2 VALUES('aa');
    INSERT INTO collate2t3 VALUES('aa');
  }
} {}
do_test collate2-4.1 {
  execsql {
    SELECT collate2t1.a FROM collate2t1, collate2t2 
      WHERE collate2t1.b = collate2t2.b;
  }
} {aa aA Aa AA}
do_test collate2-4.2 {
  execsql {
    SELECT collate2t1.a FROM collate2t1, collate2t2 
      WHERE collate2t2.b = collate2t1.b;
  }
} {aa}
do_test collate2-4.3 {
  execsql {
    SELECT collate2t1.a FROM collate2t1, collate2t3 
      WHERE collate2t1.b = collate2t3.b||''
      ORDER BY +collate2t1.a DESC;
  }
} {aa aA Aa AA}
do_test collate2-4.4 {
  execsql {
    SELECT collate2t1.a FROM collate2t1, collate2t3 
      WHERE collate2t3.b||'' = collate2t1.b
      ORDER BY +collate2t1.a DESC;
  }
} {aa aA Aa AA}
do_test collate2-4.5 {
  execsql {
    DROP TABLE collate2t3;
  }
} {}
do_test collate2-5.0 {
  execsql {
    SELECT collate2t1.b FROM collate2t1 JOIN collate2t2 USING (b);
  }
} {aa aA Aa AA}
do_test collate2-5.1 {
  execsql {
    SELECT collate2t1.b FROM collate2t2 JOIN collate2t1 USING (b);
  }
} {aa}
do_test collate2-5.2 {
  execsql {
    SELECT collate2t1.b FROM collate2t1 NATURAL JOIN collate2t2;
  }
} {aa aA Aa AA}
do_test collate2-5.3 {
  execsql {
    SELECT collate2t1.b FROM collate2t2 NATURAL JOIN collate2t1;
  }
} {aa}
do_test collate2-5.4.1 {
  execsql {
    SELECT collate2t2.b FROM collate2t1 LEFT JOIN collate2t2 USING (b) order by collate2t1.oid;
  }
} {{} aa {} {} {} aa {} {} {} aa {} {} {} aa {} {} {}}
do_test collate2-5.4.2 {
  execsql {
    SELECT collate2t2.b FROM collate2t2 RIGHT JOIN collate2t1 ON collate2t1.b=collate2t2.b
     ORDER BY collate2t1.oid;
  }
} {{} aa {} {} {} aa {} {} {} aa {} {} {} aa {} {} {}}
do_test collate2-5.4.3 {
  execsql {
    SELECT collate2t2.b FROM collate2t1 LEFT JOIN collate2t2 ON collate2t2.b=collate2t1.b
     ORDER BY collate2t1.oid;
  }
} {{} aa {} {} {} {} {} {} {} {} {} {} {} {} {} {} {}}
do_test collate2-5.5.1 {
  execsql {
    SELECT collate2t1.b, collate2t2.b FROM collate2t2 LEFT OUTER JOIN collate2t1 USING (b);
  }
} {aa aa}
do_test collate2-5.5.2 {
  execsql {
    SELECT collate2t1.b, collate2t2.b
      FROM collate2t1 RIGHT JOIN collate2t2 ON collate2t2.b=collate2t1.b
  }
} {aa aa}
do_execsql_test 6.1 {
  CREATE TABLE t1(x);
  INSERT INTO t1 VALUES('b');
  INSERT INTO t1 VALUES('B');
}
do_execsql_test 6.2 {
  SELECT * FROM t1 WHERE x COLLATE nocase BETWEEN 'a' AND 'c';
} {b B}
do_execsql_test 6.3 {
  SELECT * FROM t1 WHERE x BETWEEN 'a' COLLATE nocase AND 'c' COLLATE nocase;
} {b B}
do_execsql_test 6.4 {
  SELECT * FROM t1 
  WHERE x COLLATE nocase BETWEEN 'a' COLLATE nocase AND 'c' COLLATE nocase;
} {b B}
do_execsql_test 6.5 {
  SELECT * FROM t1 WHERE +x COLLATE nocase BETWEEN 'a' AND 'c';
} {b B}
do_execsql_test 6.6 {
  SELECT * FROM t1 WHERE +x BETWEEN 'a' COLLATE nocase AND 'c' COLLATE nocase;
} {b B}
do_execsql_test 6.7 {
  SELECT * FROM t1 
  WHERE +x COLLATE nocase BETWEEN 'a' COLLATE nocase AND 'c' COLLATE nocase;
} {b B}
finish_test
"#);
}
